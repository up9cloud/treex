//! The browser view. Same tree, same selection, same live updates — reachable
//! from a phone or tablet that has no terminal worth using.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::protocol::{Message, Role};
use tokio_tungstenite::WebSocketStream;

use http::{Body, Handled, Request, Response};

pub mod http;

use crate::preview::PreviewOptions;
use crate::state::{Command, Session};

/// Chosen to be memorable and well clear of anything else that squats on a
/// developer machine.
pub const DEFAULT_PORT: u16 = 11711;

#[derive(Debug, Clone)]
pub struct WebOptions {
    pub addr: SocketAddr,
    /// Consecutive ports to try when the first one is taken. A second treex
    /// should just come up next door rather than refuse to start.
    pub port_attempts: u16,
    /// `None` refuses to serve file contents at all.
    pub preview: Option<PreviewOptions>,
}

impl Default for WebOptions {
    fn default() -> Self {
        Self {
            addr: ([127, 0, 0, 1], DEFAULT_PORT).into(),
            port_attempts: 20,
            preview: Some(PreviewOptions::default()),
        }
    }
}

/// Binds `addr`, stepping the port forward while it is in use.
///
/// Port 0 already means "any free port", so it is never stepped.
pub async fn bind(addr: SocketAddr, attempts: u16) -> std::io::Result<tokio::net::TcpListener> {
    let base = addr.port();
    let attempts = if base == 0 { 1 } else { attempts.max(1) };

    let mut in_use = None;
    for step in 0..attempts {
        let Some(port) = base.checked_add(step) else {
            break;
        };
        let mut candidate = addr;
        candidate.set_port(port);
        match tokio::net::TcpListener::bind(candidate).await {
            Ok(listener) => return Ok(listener),
            Err(err) if err.kind() == std::io::ErrorKind::AddrInUse => in_use = Some(err),
            Err(err) => return Err(err),
        }
    }

    Err(in_use.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!(
                "ports {base}..={} are all in use",
                base.saturating_add(attempts - 1)
            ),
        )
    }))
}

/// The address the default route would leave from, which is the one a phone on
/// the same network or VPN can actually reach.
///
/// `connect` on a UDP socket sends no packets; it only asks the kernel to pick
/// a route, so this costs nothing and talks to no one.
fn outbound_ip() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind(("0.0.0.0", 0)).ok()?;
    socket.connect(("192.0.2.1", 80)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// Every URL this listener can be reached on, most useful first.
///
/// Binding `0.0.0.0` is something people do precisely because they want to
/// reach treex from another device, so printing `localhost` and nothing else
/// would answer the wrong question.
pub fn display_urls(addr: SocketAddr) -> Vec<String> {
    let port = addr.port();
    let local = format!("http://localhost:{port}");

    if addr.ip().is_unspecified() {
        match outbound_ip() {
            Some(ip) => vec![format!("http://{ip}:{port}"), local],
            None => vec![local],
        }
    } else if addr.ip().is_loopback() {
        vec![local]
    } else {
        vec![format!("http://{addr}")]
    }
}

/// The single URL worth putting in a status bar.
pub fn display_url(addr: SocketAddr) -> String {
    display_urls(addr).swap_remove(0)
}

#[derive(Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
enum ServerMsg<'a> {
    Snapshot {
        #[serde(flatten)]
        snapshot: &'a crate::state::Snapshot,
        /// Byte limit for file previews; absent when previews are off.
        preview_limit: Option<u64>,
        /// The build on this end of the socket.
        version: &'static str,
    },
    /// The tree is unchanged; only the cursor moved. Sent instead of a whole
    /// snapshot because a keypress must not cost a megabyte.
    Cursor {
        #[serde(flatten)]
        cursor: crate::state::Cursor,
    },
    /// Nothing to report. A WebSocket ping would not do: browsers answer those
    /// without telling the page, so only a frame the script can see proves the
    /// connection is still real.
    Alive,
}

#[derive(Clone)]
struct AppState {
    session: Arc<Session>,
    preview: Option<PreviewOptions>,
}

/// Binds and serves until the process ends. Returns the bound address first via
/// `on_bind`, because port 0 means the caller cannot know it in advance.
pub async fn serve(
    session: Arc<Session>,
    opts: WebOptions,
    on_bind: impl FnOnce(SocketAddr),
) -> anyhow::Result<()> {
    let listener = bind(opts.addr, opts.port_attempts).await?;
    on_bind(listener.local_addr()?);

    let state = AppState {
        session,
        preview: opts.preview,
    };
    let for_ws = state.clone();

    http::serve(
        listener,
        move |request| {
            let state = state.clone();
            async move { route(request, state) }
        },
        move |socket| {
            let state = for_ws.clone();
            async move {
                let socket = WebSocketStream::from_raw_socket(socket, Role::Server, None).await;
                client(socket, state).await;
            }
        },
    )
    .await;
    Ok(())
}

fn route(request: Request, state: AppState) -> Handled {
    if request.path == "/ws" && request.is_websocket_upgrade() {
        let key = request.header("sec-websocket-key").unwrap_or_default();
        return Handled::Upgrade(
            tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes()),
        );
    }

    let response = match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") => Response::new(200, "text/html; charset=utf-8", PAGE.clone()),
        ("GET", "/favicon.svg") => Response::new(
            200,
            "image/svg+xml",
            include_str!("../../assets/logo.svg").into(),
        ),
        ("POST", "/rpc") => rpc(&request, &state),
        ("GET", path) if path.starts_with("/f/") => return raw_file(&request, &state),
        ("GET", _) | ("POST", _) => Response::text(404, "no such route"),
        _ => Response::text(405, "method not allowed"),
    };
    Handled::Reply(response)
}

/// The one HTTP entry point for asking treex something.
///
/// Commands already travel over the WebSocket, so there is no reason for the
/// request side to grow a second vocabulary of paths and verbs. Everything that
/// is a question rather than a file is a method here.
#[derive(Deserialize)]
#[serde(tag = "method", rename_all = "camelCase")]
enum Rpc {
    /// The current tree, for a client that has not opened a socket.
    Tree,
    /// A file's contents, subject to the preview limit. `known` is the stamp
    /// the caller already holds; matching it gets `unchanged` back instead of
    /// the file all over again.
    Preview {
        path: PathBuf,
        #[serde(default)]
        known: Option<crate::preview::Stamp>,
    },
}

fn rpc(request: &Request, state: &AppState) -> Response {
    let Ok(call) = serde_json::from_slice::<Rpc>(&request.body) else {
        return Response::text(400, "unknown method");
    };

    match call {
        Rpc::Tree => json_response(&state.session.snapshot(), request),
        Rpc::Preview { path, known } => {
            let Some(opts) = state.preview else {
                return Response::text(403, "file preview is disabled");
            };
            let Some(path) = state.session.visible_file(&path) else {
                return Response::text(404, "no such file in this tree");
            };
            json_response(&crate::preview::read(&path, &opts, known), request)
        }
    }
}

/// Serves JSON, gzipped when it is worth it and the caller said it could.
fn json_response<T: Serialize>(value: &T, request: &Request) -> Response {
    let Ok(body) = serde_json::to_vec(value) else {
        return Response::text(500, "cannot serialize");
    };

    if body.len() > COMPRESS_ABOVE && request.header_has("accept-encoding", "gzip") {
        if let Ok(packed) = gzip(&body) {
            return Response::new(200, "application/json; charset=utf-8", packed)
                .with("content-encoding", "gzip");
        }
    }
    Response::new(200, "application/json; charset=utf-8", body)
}

/// Below this a message is not worth compressing: deflate has a floor of a few
/// bytes and the cursor messages are under a hundred to begin with.
const COMPRESS_ABOVE: usize = 4096;

/// Baked into the page and repeated on every connect: a reconnect can land on
/// a server that was rebuilt in between, and the corner has to follow.
const VERSION: &str = env!("CARGO_PKG_VERSION");

static PAGE: LazyLock<Vec<u8>> = LazyLock::new(|| {
    include_str!("assets/index.html")
        .replace("{{version}}", VERSION)
        .into_bytes()
});

/// How long the server stays silent before reassuring the page it is there.
const HEARTBEAT: Duration = Duration::from_secs(10);

/// Raw deflate, which is what the browser's `DecompressionStream("deflate-raw")`
/// expects. tungstenite has no permessage-deflate, so this is done a layer up —
/// which also means only the messages worth it get compressed.
fn deflate_raw(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::Write;

    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes)?;
    encoder.finish()
}

/// gzip, for HTTP, where `deflate` means something browsers disagree about.
fn gzip(bytes: &[u8]) -> std::io::Result<Vec<u8>> {
    use std::io::Write;

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes)?;
    encoder.finish()
}

/// Guessed from the extension, defaulting to plain text so source files render
/// in the tab instead of prompting a download.
fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        // Structured text gets its registered type and the browser decides what
        // to do with it. Reading it as plain text is what the preview pane is
        // for; this route is the one that hands the file over honestly.
        "json" | "map" => "application/json",
        "jsonl" | "ndjson" => "application/x-ndjson",
        "md" | "markdown" => "text/markdown; charset=utf-8",
        "toml" => "application/toml",
        "yaml" | "yml" => "application/yaml",
        "csv" => "text/csv; charset=utf-8",
        "tsv" => "text/tab-separated-values; charset=utf-8",
        "xml" => "application/xml",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" | "cjs" => "text/javascript; charset=utf-8",

        "html" | "htm" => "text/html; charset=utf-8",
        "svg" => "image/svg+xml",

        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",

        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "opus" => "audio/ogg",

        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",

        "wasm" => "application/wasm",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "bz2" => "application/x-bzip2",
        "xz" => "application/x-xz",
        "tar" => "application/x-tar",
        "bin" | "exe" | "so" | "dylib" | "o" | "a" => "application/octet-stream",

        // Source code, logs and extensionless files have no registered type of
        // their own, so plain text is the accurate answer rather than a
        // fallback — and it is the one that renders in a tab.
        _ => "text/plain; charset=utf-8",
    }
}

/// Serves a file under `/f/<path>`, so the browser can open it in a tab.
///
/// Same gate as the preview: it has to be a file the tree is showing. Unlike
/// the preview there is no size limit — the browser streams it, and refusing to
/// open a large file behind a plain link would be surprising.
fn raw_file(request: &Request, state: &AppState) -> Handled {
    let reply = |r: Response| Handled::Reply(r);

    if state.preview.is_none() {
        return reply(Response::text(403, "file contents are disabled"));
    }

    // Built segment by segment: a URL always separates with `/`, and pushing
    // the whole thing as one string would leave a Windows path holding a
    // separator the platform does not read as one.
    let mut relative = PathBuf::new();
    for segment in request.path.trim_start_matches("/f/").split('/') {
        relative.push(segment);
    }
    if relative
        .components()
        .any(|c| !matches!(c, std::path::Component::Normal(_)))
    {
        return reply(Response::text(400, "bad path"));
    }

    let Some(path) = state
        .session
        .visible_file(&state.session.root().join(relative))
    else {
        return reply(Response::text(404, "no such file in this tree"));
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return reply(Response::text(404, "cannot open it"));
    };
    let Ok(file) = std::fs::File::open(&path) else {
        return reply(Response::text(404, "cannot open it"));
    };

    Handled::Reply(Response {
        status: 200,
        headers: vec![
            ("content-type".into(), content_type(&path).into()),
            ("content-disposition".into(), "inline".into()),
            // Both HTML and SVG run scripts when opened as a top-level
            // document, and they would run against treex's own origin — able
            // to drive the tree and read every visible file. `sandbox` drops
            // them into an opaque origin with scripting off, which keeps the
            // rendering without the reach.
            (
                "content-security-policy".into(),
                "sandbox; default-src 'none'; img-src 'self' data:; style-src 'unsafe-inline'"
                    .into(),
            ),
        ],
        body: Body::File(tokio::fs::File::from_std(file), meta.len()),
    })
}

/// Reading a directory is blocking and its cost is set by the filesystem, not
/// by treex — a slow or hung network mount can take seconds. Doing that on a
/// runtime worker stalls every other request sharing it.
async fn apply_off_the_runtime(session: &Arc<Session>, cmd: Command) {
    if !cmd.may_block() {
        session.apply(cmd);
        return;
    }
    let session = session.clone();
    let _ = tokio::task::spawn_blocking(move || session.apply(cmd)).await;
}

async fn client(socket: WebSocketStream<tokio::net::TcpStream>, state: AppState) {
    let (mut tx, mut rx) = socket.split();
    let mut changed = state.session.subscribe();

    let send = {
        let session = state.session.clone();
        let preview = state.preview;
        tokio::spawn(async move {
            let mut sent_revision = u64::MAX;
            let mut sent_shape = u64::MAX;
            loop {
                // The cheap read first: it also answers whether the expensive
                // one is needed at all.
                let cursor = session.cursor();
                if cursor.revision != sent_revision {
                    let json = if cursor.shape == sent_shape {
                        serde_json::to_string(&ServerMsg::Cursor { cursor })
                    } else {
                        serde_json::to_string(&ServerMsg::Snapshot {
                            snapshot: &session.snapshot(),
                            preview_limit: preview.map(|p| p.max_bytes),
                            version: VERSION,
                        })
                    };
                    let Ok(json) = json else { break };

                    // Text below the threshold, deflate above it. The client
                    // tells the two apart by frame type, so nothing has to be
                    // negotiated.
                    let frame = if json.len() > COMPRESS_ABOVE {
                        match deflate_raw(json.as_bytes()) {
                            Ok(packed) => Message::Binary(packed.into()),
                            Err(_) => Message::Text(json.into()),
                        }
                    } else {
                        Message::Text(json.into())
                    };
                    if tx.send(frame).await.is_err() {
                        break;
                    }
                    sent_revision = cursor.revision;
                    sent_shape = cursor.shape;
                }

                // Lagging only means we missed intermediate revisions; the next
                // snapshot is current either way, so it is not an error.
                match tokio::time::timeout(HEARTBEAT, changed.recv()).await {
                    // A quiet tree is indistinguishable from a wedged socket —
                    // a slept laptop or a dropped tunnel leaves one open with
                    // no close event — so say something on the way past.
                    Err(_) => {
                        let Ok(json) = serde_json::to_string(&ServerMsg::Alive) else {
                            break;
                        };
                        if tx.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                        continue;
                    }
                    Ok(Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {}
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
                }
                // A burst of filesystem events is one visible change; draining
                // it here means one snapshot rather than one per event.
                while changed.try_recv().is_ok() {}
            }
        })
    };

    while let Some(Ok(msg)) = rx.next().await {
        let Message::Text(text) = msg else { continue };
        let Ok(cmd) = serde_json::from_str::<Command>(&text) else {
            continue;
        };
        apply_off_the_runtime(&state.session, cmd).await;
    }
    send.abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_taken_port_steps_past_what_is_in_use() {
        let held = bind(([127, 0, 0, 1], 0).into(), 1).await.unwrap();
        let taken = held.local_addr().unwrap();

        let next = bind(taken, 20).await.unwrap();
        let port = next.local_addr().unwrap().port();

        // Deliberately not `taken.port() + 1`: nothing promises the very next
        // port is free either, and on a busy machine it often is not. What is
        // promised is that it moves forward, within the attempts allowed, onto
        // something it could actually bind.
        assert_ne!(port, taken.port(), "it settled on a port already in use");
        assert!(
            (taken.port() + 1..=taken.port() + 20).contains(&port),
            "stepped from {} to {port}, past the twenty it was allowed",
            taken.port()
        );
    }

    #[tokio::test]
    async fn giving_up_reports_the_range_it_tried() {
        let held = bind(([127, 0, 0, 1], 0).into(), 1).await.unwrap();
        let addr = held.local_addr().unwrap();

        let err = bind(addr, 1).await.unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    }

    #[test]
    fn loopback_is_shown_as_localhost() {
        assert_eq!(
            display_urls(([127, 0, 0, 1], 11711).into()),
            ["http://localhost:11711"]
        );
        assert_eq!(
            display_url(([192, 168, 1, 4], 11711).into()),
            "http://192.168.1.4:11711"
        );
    }

    #[test]
    fn binding_all_interfaces_leads_with_a_reachable_address() {
        let urls = display_urls(([0, 0, 0, 0], 11711).into());
        assert!(urls.contains(&"http://localhost:11711".to_string()));

        // On a machine with a default route the LAN address must come first,
        // because that is the one another device can use.
        if let Some(ip) = outbound_ip() {
            assert_eq!(urls[0], format!("http://{ip}:11711"));
            assert_eq!(urls.len(), 2);
        }
    }
}
