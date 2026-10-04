//! `host:port` → websocket: fetch `http://host:port/json/version`, read
//! `webSocketDebuggerUrl`, connect. Used for `cdp: 9222` and for attaching
//! to an already-running browser. No HTTP client dependency: one GET over a
//! raw `TcpStream` is all the DevTools endpoint needs.

use super::ws::WsTransport;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// GET `http://host:port/json/version` and return `webSocketDebuggerUrl`.
///
/// `host` should be `localhost` or an IP literal: Chrome rejects other
/// `Host` headers (DNS-rebinding protection) unless started with
/// `--remote-allow-origins`.
pub async fn discover_ws_url(host: &str, port: u16) -> io::Result<String> {
    let body = http_get(host, port, "/json/version").await?;
    let v: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("/json/version: {e}")))?;
    v.get("webSocketDebuggerUrl")
        .and_then(|u| u.as_str())
        .map(str::to_owned)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "/json/version has no webSocketDebuggerUrl",
            )
        })
}

/// Discover the browser websocket URL for `host:port` and connect to it.
pub async fn connect(host: &str, port: u16) -> io::Result<WsTransport> {
    let url = discover_ws_url(host, port).await?;
    WsTransport::connect(&url).await
}

/// Minimal HTTP/1.1 GET returning the response body.
pub(crate) async fn http_get(host: &str, port: u16, path: &str) -> io::Result<Vec<u8>> {
    let mut stream = TcpStream::connect((host, port)).await?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;
    let mut raw = Vec::with_capacity(1024);
    stream.read_to_end(&mut raw).await?;
    parse_response(&raw)
}

/// Split status line / headers / body; check the status is 200.
fn parse_response(raw: &[u8]) -> io::Result<Vec<u8>> {
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed HTTP response"))?;
    let head = String::from_utf8_lossy(&raw[..sep]);
    let mut lines = head.lines();
    let status = lines.next().unwrap_or_default();
    let code = status
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("malformed HTTP status line: {status:?}"),
            )
        })?;
    if code != 200 {
        return Err(io::Error::other(format!(
            "DevTools endpoint answered {status}"
        )));
    }
    let mut body = raw[sep + 4..].to_vec();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                if let Ok(n) = value.trim().parse::<usize>() {
                    body.truncate(n);
                }
            }
        }
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn parses_status_and_content_length() {
        let raw =
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}junk";
        assert_eq!(parse_response(raw).unwrap(), b"{}");
        let bad = b"HTTP/1.1 404 Not Found\r\n\r\n";
        assert!(parse_response(bad).is_err());
        assert!(parse_response(b"garbage").is_err());
    }

    #[tokio::test]
    async fn discovers_from_a_fake_endpoint() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 1024];
            let n = sock.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).into_owned();
            assert!(req.starts_with("GET /json/version HTTP/1.1\r\n"), "{req}");
            let body = r#"{"Browser":"Chrome/1","webSocketDebuggerUrl":"ws://127.0.0.1:1/devtools/browser/x"}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=UTF-8\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
        });
        let url = discover_ws_url("127.0.0.1", port).await.unwrap();
        assert_eq!(url, "ws://127.0.0.1:1/devtools/browser/x");
    }
}
