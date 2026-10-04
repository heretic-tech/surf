//! A tiny HTTP forward-proxy stub for tests: plain-HTTP requests
//! (`GET http://host:port/path HTTP/1.1`) are forwarded to the origin with
//! an added `X-Surf-Proxy: <name>` request header (so the fixture's
//! `/proxy-echo` route can tell which proxy served the request) and the
//! relayed response gets an `X-Proxy: <name>` header (so an `on response`
//! hook can see it too); `CONNECT host:port` is tunnelled byte for byte.
//! Every origin host is mapped to loopback, so a script can use a
//! non-loopback host name (`surf.test`) to defeat Chrome's implicit "never
//! proxy localhost" bypass rule while the fixture server still answers.
//!
//! [`Proxy::start_with_auth`] makes the stub demand `Proxy-Authorization:
//! Basic …` (answering `407` + `Proxy-Authenticate: Basic realm="surf"`
//! otherwise), which exercises Surf's `Fetch.authRequired` handling.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// A running proxy stub. Dropping it stops the listener.
pub struct Proxy {
    /// `http://127.0.0.1:<port>` — the value for `proxies:` / `proxy:`
    /// (with `user:pass@` when authentication is required).
    pub url: String,
    /// The name the proxy tags forwarded requests with.
    pub name: String,
    hits: Arc<AtomicUsize>,
    challenges: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Proxy {
    /// Bind a free loopback port and serve without authentication. Must be
    /// called inside a tokio runtime.
    pub async fn start(name: &str) -> Proxy {
        Proxy::launch(name, None).await
    }

    /// Like [`start`](Self::start) but every request must carry
    /// `Proxy-Authorization: Basic base64(user:pass)`.
    pub async fn start_with_auth(name: &str, user: &str, pass: &str) -> Proxy {
        Proxy::launch(name, Some((user.to_owned(), pass.to_owned()))).await
    }

    async fn launch(name: &str, auth: Option<(String, String)>) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let challenges = Arc::new(AtomicUsize::new(0));
        let tag = name.to_owned();
        let counter = hits.clone();
        let challenge_counter = challenges.clone();
        let expected = auth
            .as_ref()
            .map(|(u, p)| format!("Basic {}", base64(format!("{u}:{p}").as_bytes())));
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let tag = tag.clone();
                let counter = counter.clone();
                let challenge_counter = challenge_counter.clone();
                let expected = expected.clone();
                tokio::spawn(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    // Chrome also routes its own background requests here
                    // (and nothing listens on their ports); stay quiet.
                    let _ = serve(stream, &tag, expected.as_deref(), &challenge_counter).await;
                });
            }
        });
        let url = match &auth {
            Some((u, p)) => format!("http://{u}:{p}@{addr}"),
            None => format!("http://{addr}"),
        };
        Proxy {
            url,
            name: name.to_owned(),
            hits,
            challenges,
            task,
        }
    }

    /// Connections accepted so far.
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    /// `407` challenges sent so far (authenticating proxies only).
    pub fn challenges(&self) -> usize {
        self.challenges.load(Ordering::SeqCst)
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(
    client: TcpStream,
    tag: &str,
    expected_auth: Option<&str>,
    challenges: &AtomicUsize,
) -> std::io::Result<()> {
    let (rd, mut wr) = client.into_split();
    let mut rd = BufReader::new(rd);
    loop {
        let mut request_line = String::new();
        if rd.read_line(&mut request_line).await? == 0 {
            return Ok(());
        }
        if request_line.trim().is_empty() {
            continue;
        }
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("").to_owned();
        let target = parts.next().unwrap_or("").to_owned();
        let mut headers: Vec<String> = Vec::new();
        loop {
            let mut line = String::new();
            if rd.read_line(&mut line).await? == 0 {
                return Ok(());
            }
            if line.trim_end().is_empty() {
                break;
            }
            headers.push(line.trim_end().to_owned());
        }
        let content_length: usize = header_value(&headers, "content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        if let Some(expected) = expected_auth {
            let given = header_value(&headers, "proxy-authorization");
            if given.as_deref() != Some(expected) {
                challenges.fetch_add(1, Ordering::SeqCst);
                // Drain a body so the next request on this connection parses.
                let mut skip = vec![0u8; content_length];
                if content_length > 0 {
                    rd.read_exact(&mut skip).await?;
                }
                wr.write_all(
                    b"HTTP/1.1 407 Proxy Authentication Required\r\n\
                      Proxy-Authenticate: Basic realm=\"surf\"\r\n\
                      Content-Length: 0\r\n\
                      Connection: keep-alive\r\n\r\n",
                )
                .await?;
                continue;
            }
        }
        if method.eq_ignore_ascii_case("CONNECT") {
            let port = port_of(&target).unwrap_or(443);
            let mut origin = TcpStream::connect(("127.0.0.1", port)).await?;
            wr.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await?;
            let (mut ord, mut owr) = origin.split();
            let up = async {
                let mut buf = [0u8; 8192];
                loop {
                    let n = rd.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    owr.write_all(&buf[..n]).await?;
                }
                owr.shutdown().await
            };
            let down = async {
                let mut buf = [0u8; 8192];
                loop {
                    let n = ord.read(&mut buf).await?;
                    if n == 0 {
                        break;
                    }
                    wr.write_all(&buf[..n]).await?;
                }
                wr.shutdown().await
            };
            let _ = tokio::join!(up, down);
            return Ok(());
        }
        // Absolute-form request: `GET http://host:port/path HTTP/1.1`.
        let (port, path) = match target.strip_prefix("http://") {
            Some(rest) => {
                let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
                (port_of(authority).unwrap_or(80), format!("/{path}"))
            }
            None => {
                wr.write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
                    .await?;
                return Ok(());
            }
        };
        let mut body = vec![0u8; content_length];
        if content_length > 0 {
            rd.read_exact(&mut body).await?;
        }
        let mut origin = match TcpStream::connect(("127.0.0.1", port)).await {
            Ok(o) => o,
            Err(_) => {
                wr.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n")
                    .await?;
                return Ok(());
            }
        };
        let mut out = format!("{method} {path} HTTP/1.1\r\n");
        for h in &headers {
            let key = h
                .split(':')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if matches!(
                key.as_str(),
                "proxy-connection"
                    | "proxy-authorization"
                    | "connection"
                    | "keep-alive"
                    | "x-surf-proxy"
            ) {
                continue;
            }
            out.push_str(h);
            out.push_str("\r\n");
        }
        out.push_str(&format!("X-Surf-Proxy: {tag}\r\nConnection: close\r\n\r\n"));
        origin.write_all(out.as_bytes()).await?;
        origin.write_all(&body).await?;
        // Relay the response, tagging its head with `X-Proxy`.
        let mut head = Vec::new();
        let mut buf = [0u8; 8192];
        let mut tagged = false;
        loop {
            let n = origin.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            if tagged {
                wr.write_all(&buf[..n]).await?;
                continue;
            }
            head.extend_from_slice(&buf[..n]);
            if let Some(i) = find(&head, b"\r\n\r\n") {
                let mut rewritten = Vec::with_capacity(head.len() + 32);
                rewritten.extend_from_slice(&head[..i]);
                rewritten.extend_from_slice(format!("\r\nX-Proxy: {tag}").as_bytes());
                rewritten.extend_from_slice(&head[i..]);
                wr.write_all(&rewritten).await?;
                tagged = true;
            }
        }
        if !tagged {
            wr.write_all(&head).await?;
        }
        wr.shutdown().await?;
        return Ok(());
    }
}

fn header_value(headers: &[String], name: &str) -> Option<String> {
    headers.iter().find_map(|h| {
        let (k, v) = h.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case(name)
            .then(|| v.trim().to_owned())
    })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn port_of(authority: &str) -> Option<u16> {
    authority.rsplit_once(':')?.1.parse().ok()
}

/// Standard base64 (for the expected `Proxy-Authorization` value).
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_basic_auth() {
        assert_eq!(base64(b"user:pass"), "dXNlcjpwYXNz");
    }
}
