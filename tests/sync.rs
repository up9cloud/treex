//! End-to-end checks on the promise that matters: the terminal, every browser
//! tab, and the filesystem watcher all see one tree.

#![cfg(all(feature = "web", feature = "watch"))]

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::io::Read;
use tokio_tungstenite::tungstenite::Message;
use treex::state::{Command, Session};
use treex::web::WebOptions;
use treex::ScanOptions;

async fn spawn_with(session: Arc<Session>, opts: WebOptions) -> std::net::SocketAddr {
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _ = treex::web::serve(session, opts, |bound| {
            let _ = tx.send(bound);
        })
        .await;
    });
    rx.await.unwrap()
}

async fn spawn_server(session: Arc<Session>) -> String {
    let opts = WebOptions {
        addr: ([127, 0, 0, 1], 0).into(),
        ..Default::default()
    };
    format!("ws://{}/ws", spawn_with(session, opts).await)
}

/// A bare HTTP/1.1 GET, so the guards on `/api/file` can be tested without
/// pulling a client library in just for that.
async fn http_get(addr: std::net::SocketAddr, target: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();

    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

/// A bare HTTP/1.1 POST of a JSON body.
async fn http_post(addr: std::net::SocketAddr, target: &str, body: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "POST {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    (status, body)
}

/// A POST whose whole response, headers included, comes back as text.
async fn http_post_raw(
    addr: std::net::SocketAddr,
    target: &str,
    body: &str,
    accept_encoding: Option<&str>,
) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let accept = accept_encoding
        .map(|e| format!("Accept-Encoding: {e}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
         Content-Type: application/json\r\n{accept}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    String::from_utf8_lossy(&raw).into_owned()
}

/// The whole response, headers included.
async fn http_get_raw(addr: std::net::SocketAddr, target: &str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {target} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    String::from_utf8_lossy(&raw).into_owned()
}

type Client =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(url: &str) -> Client {
    tokio_tungstenite::connect_async(url).await.unwrap().0
}

/// A frame's text, inflating it when the server judged it worth compressing.
/// The page does the same; a test that only understood one of the two would
/// quietly stop watching half the protocol.
fn frame_text(msg: Message) -> Option<String> {
    match msg {
        Message::Text(text) => Some(text.to_string()),
        Message::Binary(packed) => {
            let mut json = String::new();
            flate2::read::DeflateDecoder::new(&packed[..])
                .read_to_string(&mut json)
                .expect("a binary frame must be raw deflate");
            Some(json)
        }
        _ => None,
    }
}

/// Reads snapshots until one satisfies `pred`, so a test never depends on how
/// many intermediate revisions happened to be coalesced.
async fn wait_for(client: &mut Client, pred: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let msg = tokio::time::timeout_at(deadline, client.next())
            .await
            .expect("timed out waiting for a matching snapshot")
            .expect("socket closed")
            .unwrap();
        let Some(text) = frame_text(msg) else {
            continue;
        };
        let value: Value = serde_json::from_str(&text).unwrap();
        if pred(&value) {
            return value;
        }
    }
}

/// One decoded row. The wire carries `[depth, name, kind, flags]` and no path;
/// tests rebuild it exactly as the page does, so a protocol change cannot
/// quietly leave them asserting against a format nobody speaks.
#[derive(Debug, Clone)]
struct Line {
    depth: usize,
    name: String,
    kind: String,
    expanded: bool,
    symlink: bool,
    omitted: u64,
    path: String,
}

fn decode(raw: &[Value], root: &str) -> Vec<Line> {
    let base = if root == "/" { "" } else { root };
    let mut ancestry: Vec<String> = Vec::new();
    raw.iter()
        .map(|r| {
            let cell = r.as_array().expect("a row must be an array");
            let depth = cell[0].as_u64().unwrap() as usize;
            let name = cell[1].as_str().unwrap().to_string();
            let packed = cell[2].as_u64().unwrap();

            ancestry.truncate(depth);
            ancestry.push(name.clone());
            let path = if depth == 0 {
                root.to_string()
            } else {
                format!("{base}/{}", ancestry[1..=depth].join("/"))
            };

            Line {
                depth,
                name,
                kind: ["d", "f", "s", "p", "c", "b", "!"][(packed >> 2) as usize].to_string(),
                expanded: packed & 1 != 0,
                symlink: packed & 2 != 0,
                omitted: cell.get(3).and_then(|v| v.as_u64()).unwrap_or(0),
                path,
            }
        })
        .collect()
}

/// What a client actually knows: rows arrive in snapshots, the cursor arrives
/// in either kind of message.
#[derive(Default)]
struct View {
    rows: Vec<Line>,
    selected: Option<usize>,
    viewing: Option<usize>,
    snapshots: usize,
    cursors: usize,
}

impl View {
    fn apply(&mut self, msg: &Value) {
        let idx = |v: &Value| v.as_u64().map(|n| n as usize);
        match msg["type"].as_str() {
            Some("snapshot") => {
                let root = msg["root"].as_str().unwrap_or_default();
                self.rows = decode(msg["rows"].as_array().unwrap(), root);
                self.selected = idx(&msg["selected"]);
                self.viewing = idx(&msg["viewing"]);
                self.snapshots += 1;
            }
            Some("cursor") => {
                self.selected = idx(&msg["selected"]);
                self.viewing = idx(&msg["viewing"]);
                self.cursors += 1;
            }
            _ => {}
        }
    }

    fn at(&self, index: Option<usize>) -> Option<&Line> {
        index.and_then(|i| self.rows.get(i))
    }

    fn named(&self, name: &str) -> Option<&Line> {
        self.rows.iter().find(|r| r.name == name)
    }

    fn names(&self) -> Vec<String> {
        self.rows.iter().map(|r| r.name.clone()).collect()
    }
}

/// Feeds messages into `view` until it satisfies `pred`.
async fn wait_view(client: &mut Client, view: &mut View, pred: impl Fn(&View) -> bool) {
    if pred(view) {
        return;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let msg = tokio::time::timeout_at(deadline, client.next())
            .await
            .expect("timed out waiting for the view to settle")
            .expect("socket closed")
            .unwrap();
        let Some(text) = frame_text(msg) else {
            continue;
        };
        view.apply(&serde_json::from_str(&text).unwrap());
        if pred(view) {
            return;
        }
    }
}

/// Opens a socket and reads until the view satisfies `pred`.
async fn view_of(url: &str) -> (Client, View) {
    let mut client = connect(url).await;
    let mut view = View::default();
    wait_view(&mut client, &mut view, |v| !v.rows.is_empty()).await;
    (client, view)
}

#[tokio::test]
async fn a_command_from_one_browser_tab_reaches_another() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("sub/inner.txt"), "x").unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let url = spawn_server(session.clone()).await;

    let (mut a, mut view_a) = view_of(&url).await;
    let (mut b, mut view_b) = view_of(&url).await;

    let sub = session.root().join("sub");
    a.send(Message::Text(
        serde_json::to_string(&Command::Toggle { path: sub })
            .unwrap()
            .into(),
    ))
    .await
    .unwrap();

    wait_view(&mut b, &mut view_b, |v| v.named("inner.txt").is_some()).await;
    assert!(view_b.named("sub").unwrap().expanded);

    wait_view(&mut a, &mut view_a, |v| v.named("inner.txt").is_some()).await;
    assert_eq!(view_a.selected, view_b.selected);
}

#[tokio::test]
async fn the_terminal_and_the_browser_share_one_selection() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();
    std::fs::write(dir.path().join("b.txt"), "x").unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let url = spawn_server(session.clone()).await;
    let (mut browser, mut view) = view_of(&url).await;

    // Exactly what a key press in the TUI does.
    session.apply(Command::MoveSelection { delta: 2 });

    wait_view(&mut browser, &mut view, |v| v.selected == Some(2)).await;
    assert_eq!(view.at(view.selected).unwrap().name, "b.txt");
}

#[tokio::test]
async fn files_appearing_on_disk_reach_the_browser_unprompted() {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let _watcher = treex::watch::watch(session.clone(), Duration::from_millis(50)).unwrap();

    let url = spawn_server(session.clone()).await;
    let (mut browser, mut view) = view_of(&url).await;

    std::fs::write(dir.path().join("appeared.txt"), "x").unwrap();
    wait_view(&mut browser, &mut view, |v| {
        v.named("appeared.txt").is_some()
    })
    .await;

    std::fs::remove_file(dir.path().join("appeared.txt")).unwrap();
    wait_view(&mut browser, &mut view, |v| {
        v.named("appeared.txt").is_none()
    })
    .await;
    assert!(!view.names().contains(&"appeared.txt".to_string()));
}

/// The page reads rows positionally and rebuilds every path from depth-first
/// order. Both halves of that are load-bearing and neither fails loudly.
#[tokio::test]
async fn rows_are_positional_and_carry_no_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("sub/deeper")).unwrap();
    std::fs::write(dir.path().join("sub/deeper/leaf.txt"), "x").unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    session.apply(Command::ExpandDepth { depth: 9 });
    let url = spawn_server(session.clone()).await;

    let mut client = connect(&url).await;
    let snapshot = wait_for(&mut client, |s| s["type"] == "snapshot").await;

    for key in [
        "type",
        "revision",
        "root",
        "rows",
        "selected",
        "viewing",
        "showHidden",
    ] {
        assert!(snapshot.get(key).is_some(), "snapshot is missing {key}");
    }

    let raw = snapshot["rows"].as_array().unwrap();
    let first = raw[0]
        .as_array()
        .expect("a row must be an array, not an object");
    assert_eq!(first.len(), 3, "depth, name, packed");
    assert_eq!(first[0], 0);
    // Kind 0 (dir) shifted up two, with the expanded bit set.
    assert_eq!(first[2], 1);

    // No row carries a path; the client rebuilds them.
    for row in raw {
        for cell in row.as_array().unwrap() {
            if let Some(text) = cell.as_str() {
                assert!(!text.contains('/'), "a path leaked onto the wire: {text}");
            }
        }
    }

    let decoded = decode(raw, snapshot["root"].as_str().unwrap());
    let leaf = decoded.iter().find(|r| r.name == "leaf.txt").unwrap();
    assert_eq!(leaf.depth, 3);
    assert_eq!(leaf.kind, "f");
    assert!(!leaf.symlink);
    assert_eq!(leaf.omitted, 0);
    assert_eq!(
        leaf.path,
        session.root().join("sub/deeper/leaf.txt").to_string_lossy(),
        "rebuilt path does not match the real one"
    );
}

/// Only drawn directories are watched, so the watch for a subdirectory has to
/// be registered at the moment it is expanded — not up front.
#[tokio::test]
async fn expanding_a_directory_starts_watching_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let _watcher = treex::watch::watch(session.clone(), Duration::from_millis(50)).unwrap();
    let url = spawn_server(session.clone()).await;
    let (mut browser, mut view) = view_of(&url).await;

    // While `sub` is collapsed nothing inside it is drawn, so nothing watches it.
    std::fs::write(dir.path().join("sub/before.txt"), "x").unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(view.named("before.txt").is_none());

    session.apply(Command::Expand {
        path: session.root().join("sub"),
    });

    // Expanding re-reads, so the file written while it was shut is there...
    wait_view(&mut browser, &mut view, |v| v.named("before.txt").is_some()).await;

    // ...and the directory is now watched, with no further prompting.
    std::fs::write(dir.path().join("sub/after.txt"), "x").unwrap();
    wait_view(&mut browser, &mut view, |v| v.named("after.txt").is_some()).await;
}

/// `/api/file` serves contents, so its authorization has to be airtight: the
/// server may well be bound to 0.0.0.0 with no other gate in front of it.
#[tokio::test]
async fn file_contents_are_served_only_for_files_in_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("shut")).unwrap();
    std::fs::write(dir.path().join("shut/hidden-away.txt"), "secret").unwrap();
    std::fs::write(dir.path().join("visible.txt"), "hello").unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let root = session.root();
    let addr = spawn_with(
        session.clone(),
        WebOptions {
            addr: ([127, 0, 0, 1], 0).into(),
            ..Default::default()
        },
    )
    .await;

    let get = |p: std::path::PathBuf| async move {
        let body = serde_json::json!({ "method": "preview", "path": p }).to_string();
        http_post(addr, "/rpc", &body).await
    };

    let (status, body) = get(root.join("visible.txt")).await;
    assert_eq!(status, 200);
    assert!(body.contains("hello"), "{body}");

    // Absolute path outside the root.
    let (status, _) = get("/etc/passwd".into()).await;
    assert_eq!(status, 404);

    // Traversal back out of the root.
    let (status, _) = get(root.join("../../../etc/passwd")).await;
    assert_eq!(status, 404);

    // A directory is not a file.
    let (status, _) = get(root.join("shut")).await;
    assert_eq!(status, 404);

    // Inside a directory nobody has expanded, so it is not in the tree at all.
    let (status, body) = get(root.join("shut/hidden-away.txt")).await;
    assert_eq!(status, 404);
    assert!(!body.contains("secret"), "{body}");

    // ...and it becomes readable exactly when it becomes visible.
    session.apply(Command::Expand {
        path: root.join("shut"),
    });
    let (status, body) = get(root.join("shut/hidden-away.txt")).await;
    assert_eq!(status, 200);
    assert!(body.contains("secret"), "{body}");
}

#[tokio::test]
async fn an_oversized_file_reports_the_limit_instead_of_its_contents() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("big.txt"), "A".repeat(4096)).unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let root = session.root();
    let addr = spawn_with(
        session,
        WebOptions {
            addr: ([127, 0, 0, 1], 0).into(),
            preview: Some(treex::PreviewOptions { max_bytes: 1024 }),
            ..Default::default()
        },
    )
    .await;

    let call = serde_json::json!({ "method": "preview", "path": root.join("big.txt") });
    let (status, body) = http_post(addr, "/rpc", &call.to_string()).await;
    assert_eq!(status, 200);
    assert!(body.contains("tooLarge"), "{body}");
    assert!(
        !body.contains("AAAA"),
        "contents leaked past the limit: {body}"
    );
}

#[tokio::test]
async fn previews_can_be_switched_off_entirely() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let root = session.root();
    let addr = spawn_with(
        session,
        WebOptions {
            addr: ([127, 0, 0, 1], 0).into(),
            preview: None,
            ..Default::default()
        },
    )
    .await;

    let call = serde_json::json!({ "method": "preview", "path": root.join("a.txt") });
    let (status, body) = http_post(addr, "/rpc", &call.to_string()).await;
    assert_eq!(status, 403);
    assert!(!body.contains("hello"), "{body}");
}

/// Reading a file is a second step on top of the cursor, and any cursor
/// movement is a way back out of it.
#[tokio::test]
async fn the_cursor_moves_out_of_whatever_was_being_read() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("a.txt"), "first").unwrap();
    std::fs::write(dir.path().join("b.txt"), "second").unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let url = spawn_server(session.clone()).await;
    let (mut browser, mut view) = view_of(&url).await;

    // Cursor onto the file, then Enter.
    session.apply(Command::Select {
        path: session.root().join("a.txt"),
    });
    wait_view(&mut browser, &mut view, |v| {
        v.at(v.selected).is_some_and(|r| r.name == "a.txt")
    })
    .await;
    assert_eq!(view.viewing, None, "the cursor alone opened a file");

    session.apply(Command::View {
        path: Some(session.root().join("a.txt")),
    });
    wait_view(&mut browser, &mut view, |v| {
        v.at(v.viewing).is_some_and(|r| r.name == "a.txt")
    })
    .await;

    // An arrow key is a way out.
    session.apply(Command::Select {
        path: session.root().join("b.txt"),
    });
    wait_view(&mut browser, &mut view, |v| {
        v.at(v.selected).is_some_and(|r| r.name == "b.txt")
    })
    .await;
    assert_eq!(view.viewing, None);

    // ...and so is landing on a directory.
    session.apply(Command::View {
        path: Some(session.root().join("b.txt")),
    });
    wait_view(&mut browser, &mut view, |v| v.viewing.is_some()).await;
    session.apply(Command::Select {
        path: session.root().join("sub"),
    });
    wait_view(&mut browser, &mut view, |v| {
        v.at(v.selected).is_some_and(|r| r.name == "sub")
    })
    .await;
    assert_eq!(view.viewing, None);

    // None of that reshaped the tree, so none of it resent the rows.
    assert_eq!(view.snapshots, 1, "the tree was resent for a cursor move");
    assert!(view.cursors >= 5);
}

/// A click in the browser is one step, not two: it reads the file and takes
/// the cursor with it.
#[tokio::test]
async fn opening_from_the_browser_also_moves_the_cursor() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("sub")).unwrap();
    std::fs::write(dir.path().join("a.txt"), "x").unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let url = spawn_server(session.clone()).await;
    let mut browser = connect(&url).await;
    wait_for(&mut browser, |s| s["type"] == "snapshot").await;

    let file = session.root().join("a.txt");
    browser
        .send(Message::Text(
            serde_json::to_string(&Command::View {
                path: Some(file.clone()),
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snap = session.snapshot();
        let selected = snap.selected.map(|i| snap.rows[i].path.to_path_buf());
        if selected == Some(file.clone()) && snap.viewing.is_some() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "cursor never followed"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // A directory can never be the thing on show.
    session.apply(Command::View {
        path: Some(session.root().join("sub")),
    });
    assert_eq!(session.snapshot().viewing, None);
}

/// Hiding dotfiles is shared state, not a per-view preference: `.` in the
/// terminal and the button in the browser are the same switch.
#[tokio::test]
async fn either_side_can_hide_dotfiles_and_both_see_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(".env"), "x").unwrap();
    std::fs::write(dir.path().join("visible.txt"), "x").unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let url = spawn_server(session.clone()).await;
    let (mut browser, mut view) = view_of(&url).await;
    assert!(
        view.named(".env").is_some(),
        "dotfiles are shown by default"
    );

    // The browser's button.
    browser
        .send(Message::Text(
            serde_json::to_string(&Command::SetHidden { show: false })
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
    wait_view(&mut browser, &mut view, |v| v.named(".env").is_none()).await;
    assert!(view.named("visible.txt").is_some());

    // `.` in the terminal.
    session.apply(Command::SetHidden { show: true });
    wait_view(&mut browser, &mut view, |v| v.named(".env").is_some()).await;
}

/// Files are also served under their own URL, so the browser can open one in a
/// tab. Same gate as the preview, and the same traversal story.
#[tokio::test]
async fn files_are_served_under_their_own_path() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("shut")).unwrap();
    std::fs::write(dir.path().join("shut/inner.txt"), "inner").unwrap();
    std::fs::write(dir.path().join("page.html"), "<b>hi</b>").unwrap();
    std::fs::write(dir.path().join("pic.png"), [0x89, b'P', b'N', b'G']).unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let addr = spawn_with(
        session.clone(),
        WebOptions {
            addr: ([127, 0, 0, 1], 0).into(),
            ..Default::default()
        },
    )
    .await;

    let (status, body) = http_get(addr, "/f/page.html").await;
    assert_eq!(status, 200);
    assert!(body.contains("<b>hi</b>"), "{body}");

    // Unknown and text-ish extensions render in the tab rather than downloading.
    let (_, raw) = http_get(addr, "/f/pic.png").await;
    assert!(!raw.is_empty());

    // The prefix is what keeps awkwardly named files reachable.
    std::fs::write(dir.path().join("ws"), "a file called ws").unwrap();
    session.apply(Command::Refresh);
    let (status, body) = http_get(addr, "/f/ws").await;
    assert_eq!(status, 200);
    assert!(body.contains("a file called ws"), "{body}");

    assert_eq!(http_get(addr, "/").await.0, 200);
    assert_eq!(http_post(addr, "/rpc", r#"{"method":"tree"}"#).await.0, 200);
    assert_eq!(http_get(addr, "/favicon.svg").await.0, 200);

    // Everything the preview refuses, this refuses too. A literal `..` never
    // reaches the tree lookup: it is rejected as a malformed path first, which
    // is why this is 400 rather than 404.
    let (status, body) = http_get(addr, "/f/../../../etc/passwd").await;
    assert_eq!(status, 400);
    assert!(!body.contains("root:"), "{body}");
    // A client that normalized the path for us lands on the tree check instead.
    assert_eq!(http_get(addr, "/f/etc/passwd").await.0, 404);
    assert_eq!(http_get(addr, "/f/shut").await.0, 404);
    let (status, body) = http_get(addr, "/f/shut/inner.txt").await;
    assert_eq!(status, 404, "a file in a collapsed directory was served");
    assert!(!body.contains("inner"));

    session.apply(Command::Expand {
        path: session.root().join("shut"),
    });
    let (status, body) = http_get(addr, "/f/shut/inner.txt").await;
    assert_eq!(status, 200);
    assert!(body.contains("inner"), "{body}");
}

#[tokio::test]
async fn no_preview_also_closes_the_static_route() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.txt"), "hello").unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let addr = spawn_with(
        session,
        WebOptions {
            addr: ([127, 0, 0, 1], 0).into(),
            preview: None,
            ..Default::default()
        },
    )
    .await;

    let (status, body) = http_get(addr, "/f/a.txt").await;
    assert_eq!(status, 403);
    assert!(!body.contains("hello"), "{body}");
}

/// HTML and SVG execute scripts when opened as a top-level document. Served
/// from treex's own origin they could drive the tree and read every visible
/// file, so every raw response is sandboxed.
#[tokio::test]
async fn raw_responses_cannot_script_against_treex() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("evil.html"),
        "<script>fetch('/api/tree')</script>",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("evil.svg"),
        "<svg xmlns='http://www.w3.org/2000/svg'/>",
    )
    .unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let addr = spawn_with(
        session,
        WebOptions {
            addr: ([127, 0, 0, 1], 0).into(),
            ..Default::default()
        },
    )
    .await;

    for target in ["/f/evil.html", "/f/evil.svg"] {
        let raw = http_get_raw(addr, target).await;
        assert!(raw.starts_with("HTTP/1.1 200"), "{raw}");
        let headers = raw.to_ascii_lowercase();
        assert!(
            headers.contains("content-security-policy: sandbox"),
            "{target} was served without a sandbox"
        );
    }
}

/// Extensions the browser can do something better than plain text with.
#[tokio::test]
async fn structured_text_gets_its_own_content_type() {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in [
        ("a.json", "{}"),
        ("a.yaml", "k: v"),
        ("a.yml", "k: v"),
        ("a.csv", "a,b"),
        ("a.xml", "<x/>"),
        ("a.css", "b{}"),
        ("a.js", "1"),
        ("a.rs", "fn main() {}"),
        ("a.toml", "k = 1"),
        ("a.md", "# hi"),
        ("noext", "hello"),
    ] {
        std::fs::write(dir.path().join(name), body).unwrap();
    }

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let addr = spawn_with(
        session,
        WebOptions {
            addr: ([127, 0, 0, 1], 0).into(),
            ..Default::default()
        },
    )
    .await;

    for (name, expected) in [
        ("a.json", "application/json"),
        ("a.yaml", "application/yaml"),
        ("a.yml", "application/yaml"),
        ("a.csv", "text/csv; charset=utf-8"),
        ("a.xml", "application/xml"),
        ("a.css", "text/css; charset=utf-8"),
        ("a.js", "text/javascript; charset=utf-8"),
        ("a.md", "text/markdown; charset=utf-8"),
        ("a.toml", "application/toml"),
        // No registered type exists for these, so plain text is the accurate
        // answer, not a cop-out.
        ("a.rs", "text/plain; charset=utf-8"),
        ("noext", "text/plain; charset=utf-8"),
    ] {
        let raw = http_get_raw(addr, &format!("/f/{name}"))
            .await
            .to_lowercase();
        assert!(
            raw.contains(&format!("content-type: {expected}")),
            "{name} was not served as {expected}"
        );
    }
}

/// Moving the cursor must not cost a whole tree. On a large checkout the full
/// snapshot runs to hundreds of kilobytes, and a keypress that resends it is
/// unusable over a phone connection — which is the case this project exists for.
#[tokio::test]
async fn a_cursor_move_does_not_resend_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("deep")).unwrap();
    for i in 0..300 {
        std::fs::write(dir.path().join(format!("deep/file-{i:04}.txt")), "x").unwrap();
    }

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    session.apply(Command::ExpandDepth { depth: 2 });
    let url = spawn_server(session.clone()).await;
    let mut browser = connect(&url).await;

    let first = wait_for(&mut browser, |s| s["type"] == "snapshot").await;
    // The uncompressed size is the fair comparison: the point is that the
    // cursor message does not carry the rows, not that deflate is good.
    let full = serde_json::to_string(&first).unwrap().len();
    assert!(first["rows"].as_array().unwrap().len() > 300);

    session.apply(Command::MoveSelection { delta: 1 });
    let msg = wait_for(&mut browser, |s| s["type"] == "cursor").await;

    assert_eq!(msg["selected"], 1);
    assert!(msg["rows"].is_null(), "the rows came along anyway: {msg}");
    let moved = serde_json::to_string(&msg).unwrap().len();
    assert!(
        moved * 50 < full,
        "a cursor move cost {moved} bytes against a {full}-byte snapshot"
    );

    // Reshaping still sends everything, because the client has nothing else.
    session.apply(Command::CollapseAll);
    let after = wait_for(&mut browser, |s| s["type"] == "snapshot").await;
    assert_eq!(
        after["rows"].as_array().unwrap().len(),
        2,
        "root and `deep`"
    );
}

/// tungstenite has no permessage-deflate, so compression is done a layer up:
/// large snapshots go out deflated in a binary frame, small ones as text.
#[tokio::test]
async fn large_snapshots_are_compressed_and_small_ones_are_not() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("deep")).unwrap();
    for i in 0..400 {
        std::fs::write(dir.path().join(format!("deep/file-{i:04}.txt")), "x").unwrap();
    }

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    session.apply(Command::ExpandDepth { depth: 2 });
    let url = spawn_server(session.clone()).await;
    let mut client = connect(&url).await;

    // The opening snapshot is well past the threshold.
    let packed = loop {
        match client.next().await.unwrap().unwrap() {
            Message::Binary(bytes) => break bytes,
            Message::Text(text) => panic!("a {}-byte snapshot went out raw", text.len()),
            _ => continue,
        }
    };

    let mut json = String::new();
    flate2::read::DeflateDecoder::new(&packed[..])
        .read_to_string(&mut json)
        .expect("the frame must be raw deflate, as DecompressionStream expects");

    let snapshot: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(snapshot["type"], "snapshot");
    assert!(snapshot["rows"].as_array().unwrap().len() > 400);
    assert!(
        packed.len() * 2 < json.len(),
        "compression bought nothing: {} vs {}",
        packed.len(),
        json.len()
    );

    // A cursor move is far too small to be worth it and stays readable.
    session.apply(Command::MoveSelection { delta: 1 });
    loop {
        match client.next().await.unwrap().unwrap() {
            Message::Text(text) => {
                assert!(text.contains("\"cursor\""), "{text}");
                break;
            }
            Message::Binary(_) => panic!("a cursor message was compressed"),
            _ => continue,
        }
    }
}

/// Previews travel over HTTP, where gzip is the encoding browsers agree on.
#[tokio::test]
async fn large_previews_are_gzipped_when_the_caller_accepts_it() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("big.txt"), "treex ".repeat(4000)).unwrap();
    std::fs::write(dir.path().join("small.txt"), "hello").unwrap();

    let session = Session::new(dir.path(), ScanOptions::default()).unwrap();
    let root = session.root();
    let addr = spawn_with(
        session,
        WebOptions {
            addr: ([127, 0, 0, 1], 0).into(),
            ..Default::default()
        },
    )
    .await;

    let call =
        |name: &str| serde_json::json!({"method": "preview", "path": root.join(name)}).to_string();

    let plain = http_post_raw(addr, "/rpc", &call("big.txt"), None).await;
    assert!(!plain.to_lowercase().contains("content-encoding"));

    let packed = http_post_raw(addr, "/rpc", &call("big.txt"), Some("gzip")).await;
    assert!(
        packed.to_lowercase().contains("content-encoding: gzip"),
        "a large preview was not compressed"
    );

    // Below the threshold it is not worth the bytes it would add.
    let small = http_post_raw(addr, "/rpc", &call("small.txt"), Some("gzip")).await;
    assert!(!small.to_lowercase().contains("content-encoding"));
}
