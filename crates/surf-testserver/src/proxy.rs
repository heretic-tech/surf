//! A tiny HTTP forward-proxy stub for tests: plain-HTTP requests
//! (`GET http://host:port/path HTTP/1.1`) are forwarded to the origin with
//! an added `X-Surf-Proxy: <name>` header so the fixture's `/proxy-echo`
//! route can tell which proxy served the request; `CONNECT host:port` is
//! tunnelled byte for byte. Every origin host is mapped to loopback, so a
//! script can use a non-loopback host name (`surf.test`) to defeat
//! Chrome's implicit "never proxy localhost" bypass rule while the fixture
//! server still answers.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// A running proxy stub. Dropping it stops the listener.
pub struct Proxy {
    /// `http://127.0.0.1:<port>` — the value for `proxies:` / `proxy:`.
    pub url: String,
    /// The name the proxy tags forwarded requests with.
    pub name: String,
    hits: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Proxy {
    /// Bind a free loopback port and serve. Must be called inside a tokio
    /// runtime.
    pub async fn start(name: &str) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let tag = name.to_owned();
        let counter = hits.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let tag = tag.clone();
                let counter = counter.clone();
                tokio::spawn(async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    // Chrome also routes its own background requests here
                    // (and nothing listens on their ports); stay quiet.
                    let _ = serve(stream, &tag).await;
                });
            }
        });
        Proxy {
            url: format!("http://{addr}"),
            name: name.to_owned(),
            hits,
            task,
        }
    }

    /// Connections accepted so far.
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn serve(client: TcpStream, tag: &str) -> std::io::Result<()> {
    let (rd, mut wr) = client.into_split();
    let mut rd = BufReader::new(rd);
    let mut request_line = String::new();
    if rd.read_line(&mut request_line).await? == 0 {
        return Ok(());
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
    let content_length: usize = headers
        .iter()
        .find_map(|h| {
            let (k, v) = h.split_once(':')?;
            k.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse().ok())
                .flatten()
        })
        .unwrap_or(0);
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
            "proxy-connection" | "connection" | "keep-alive" | "x-surf-proxy"
        ) {
            continue;
        }
        out.push_str(h);
        out.push_str("\r\n");
    }
    out.push_str(&format!("X-Surf-Proxy: {tag}\r\nConnection: close\r\n\r\n"));
    origin.write_all(out.as_bytes()).await?;
    origin.write_all(&body).await?;
    let mut buf = [0u8; 8192];
    loop {
        let n = origin.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        wr.write_all(&buf[..n]).await?;
    }
    wr.shutdown().await?;
    Ok(())
}

fn port_of(authority: &str) -> Option<u16> {
    authority.rsplit_once(':')?.1.parse().ok()
}
