//! A minimal HTTP/1.1 server, enough for five routes.
//!
//! This replaced axum and hyper. The trade is deliberate: twenty fewer crates
//! under a tool whose whole job is to read your files, against a few hundred
//! lines that have to be right. Request parsing is `httparse`, which is already
//! here for the WebSocket handshake — nothing about framing is hand-rolled.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Caps, so one rude connection cannot hold memory or a task open.
const MAX_HEAD: usize = 16 * 1024;
const MAX_BODY: usize = 1024 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(15);

pub struct Request {
    pub method: String,
    /// Path with the query string removed and percent-escapes decoded.
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    /// Whether `name` lists `token`, case-insensitively — the form most of the
    /// interesting HTTP headers take.
    pub fn header_has(&self, name: &str, token: &str) -> bool {
        self.header(name)
            .is_some_and(|v| v.to_ascii_lowercase().contains(token))
    }

    pub fn is_websocket_upgrade(&self) -> bool {
        self.header_has("connection", "upgrade")
            && self.header_has("upgrade", "websocket")
            && self.header("sec-websocket-key").is_some()
    }
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Body,
}

pub enum Body {
    Bytes(Vec<u8>),
    /// A file to send, with the length promised in `Content-Length`.
    File(tokio::fs::File, u64),
}

impl Response {
    pub fn new(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Self {
            status,
            headers: vec![("content-type".into(), content_type.into())],
            body: Body::Bytes(body),
        }
    }

    pub fn text(status: u16, message: &str) -> Self {
        Self::new(status, "text/plain; charset=utf-8", message.into())
    }

    pub fn with(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        500 => "Internal Server Error",
        _ => "OK",
    }
}

/// Percent-decoding, which is all of a URL path treex has any business reading.
fn decode_path(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&raw[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Reads one request head, or `None` at a clean end of connection.
async fn read_request(stream: &mut BufReader<TcpStream>) -> std::io::Result<Option<Request>> {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if buf.len() > MAX_HEAD {
            return Err(std::io::Error::other("request head too large"));
        }
        let mut chunk = [0u8; 2048];
        let n = match tokio::time::timeout(HEAD_TIMEOUT, stream.read(&mut chunk)).await {
            Ok(Ok(0)) if buf.is_empty() => return Ok(None),
            Ok(Ok(0)) => return Err(std::io::Error::other("truncated request")),
            Ok(Ok(n)) => n,
            Ok(Err(err)) => return Err(err),
            Err(_) => return Err(std::io::Error::other("timed out reading the request")),
        };
        buf.extend_from_slice(&chunk[..n]);
    };

    let mut header_slots = [httparse::EMPTY_HEADER; 48];
    let mut parsed = httparse::Request::new(&mut header_slots);
    if parsed.parse(&buf).is_err() {
        return Err(std::io::Error::other("malformed request"));
    }

    let target = parsed.path.unwrap_or("/");
    let path = decode_path(target.split(['?', '#']).next().unwrap_or("/"));
    let headers: HashMap<String, String> = parsed
        .headers
        .iter()
        .map(|h| {
            (
                h.name.to_ascii_lowercase(),
                String::from_utf8_lossy(h.value).trim().to_string(),
            )
        })
        .collect();

    let length: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if length > MAX_BODY {
        return Err(std::io::Error::other("body too large"));
    }

    let mut body = buf[head_end..].to_vec();
    body.truncate(length);
    while body.len() < length {
        let mut chunk = vec![0u8; (length - body.len()).min(8192)];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::other("truncated body"));
        }
        body.extend_from_slice(&chunk[..n]);
    }

    Ok(Some(Request {
        method: parsed.method.unwrap_or("GET").to_string(),
        path,
        headers,
        body,
    }))
}

async fn write_response(
    stream: &mut BufReader<TcpStream>,
    response: Response,
    keep_alive: bool,
) -> std::io::Result<()> {
    let length = match &response.body {
        Body::Bytes(bytes) => bytes.len() as u64,
        Body::File(_, len) => *len,
    };

    let mut head = format!(
        "HTTP/1.1 {} {}\r\n",
        response.status,
        reason(response.status)
    );
    for (name, value) in &response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("content-length: {length}\r\n"));
    head.push_str(if keep_alive {
        "connection: keep-alive\r\n\r\n"
    } else {
        "connection: close\r\n\r\n"
    });
    stream.get_mut().write_all(head.as_bytes()).await?;

    match response.body {
        Body::Bytes(bytes) => stream.get_mut().write_all(&bytes).await?,
        Body::File(mut file, len) => {
            // Exactly `len` bytes, whatever the file does underneath: the
            // promise in Content-Length is what keeps the connection usable.
            let mut sent = 0u64;
            let mut chunk = vec![0u8; 64 * 1024];
            while sent < len {
                let want = ((len - sent) as usize).min(chunk.len());
                let n = file.read(&mut chunk[..want]).await?;
                if n == 0 {
                    chunk[..want].fill(0);
                    stream.get_mut().write_all(&chunk[..want]).await?;
                    break;
                }
                stream.get_mut().write_all(&chunk[..n]).await?;
                sent += n as u64;
            }
        }
    }
    stream.get_mut().flush().await
}

/// What a handler can decide to do with a request.
pub enum Handled {
    Reply(Response),
    /// Take the connection over as a WebSocket, with this accept key.
    Upgrade(String),
}

/// Serves connections until the listener dies.
pub async fn serve<H, F, W, G>(listener: tokio::net::TcpListener, handle: H, websocket: W)
where
    H: Fn(Request) -> F + Clone + Send + 'static,
    F: std::future::Future<Output = Handled> + Send,
    W: Fn(TcpStream) -> G + Clone + Send + 'static,
    G: std::future::Future<Output = ()> + Send,
{
    loop {
        let Ok((socket, _peer)) = listener.accept().await else {
            continue;
        };
        let handle = handle.clone();
        let websocket = websocket.clone();
        tokio::spawn(async move {
            let _ = connection(socket, handle, websocket).await;
        });
    }
}

async fn connection<H, F, W, G>(socket: TcpStream, handle: H, websocket: W) -> std::io::Result<()>
where
    H: Fn(Request) -> F,
    F: std::future::Future<Output = Handled>,
    W: Fn(TcpStream) -> G,
    G: std::future::Future<Output = ()>,
{
    let _ = socket.set_nodelay(true);
    let mut stream = BufReader::new(socket);

    loop {
        let Some(request) = read_request(&mut stream).await? else {
            return Ok(());
        };
        let keep_alive = !request.header_has("connection", "close");

        match handle(request).await {
            Handled::Reply(response) => {
                write_response(&mut stream, response, keep_alive).await?;
                if !keep_alive {
                    return Ok(());
                }
            }
            Handled::Upgrade(accept) => {
                let head = format!(
                    "HTTP/1.1 101 Switching Protocols\r\n\
                     upgrade: websocket\r\nconnection: Upgrade\r\n\
                     sec-websocket-accept: {accept}\r\n\r\n"
                );
                stream.get_mut().write_all(head.as_bytes()).await?;
                stream.get_mut().flush().await?;
                websocket(stream.into_inner()).await;
                return Ok(());
            }
        }
    }
}

pub fn local_addr(listener: &tokio::net::TcpListener) -> std::io::Result<SocketAddr> {
    listener.local_addr()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_escapes_come_back() {
        assert_eq!(decode_path("/f/a%20b.txt"), "/f/a b.txt");
        assert_eq!(decode_path("/f/hash%23q%3F.txt"), "/f/hash#q?.txt");
        assert_eq!(
            decode_path("/f/%E4%B8%AD%E6%96%87.txt"),
            "/f/\u{4e2d}\u{6587}.txt"
        );
        // A stray percent is data, not a parse error.
        assert_eq!(decode_path("/f/100%"), "/f/100%");
        assert_eq!(decode_path("/f/%zz"), "/f/%zz");
    }
}
