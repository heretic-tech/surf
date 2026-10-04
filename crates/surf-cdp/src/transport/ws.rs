//! WebSocket transport — only for `cdp: 9222`, `cdp: "ws://…"` and
//! `pool: "wss://…"`. Never the default.
//!
//! One CDP message is one **text** frame. Size limits are lifted
//! (screenshots and `Page.captureSnapshot` payloads can be tens of MiB).
//! Ping frames are answered by `tokio-tungstenite` automatically on the
//! next read; binary frames are passed through as bytes; close frames end
//! the stream (`recv` → `Ok(None)`).

use super::Transport;
use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use std::io;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Stream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// A `tokio-tungstenite` backed transport.
pub struct WsTransport {
    url: String,
    stream: Stream,
}

/// Previous name of [`WsTransport`].
pub type WebSocketTransport = WsTransport;

impl WsTransport {
    /// Connect to a DevTools websocket URL
    /// (`ws://host:port/devtools/browser/<id>` or a `wss://` pool URL).
    pub async fn connect(url: &str) -> io::Result<Self> {
        let config = WebSocketConfig::default()
            .max_message_size(None)
            .max_frame_size(None);
        let (stream, _response) =
            tokio_tungstenite::connect_async_with_config(url, Some(config), true)
                .await
                .map_err(ws_err)?;
        Ok(Self {
            url: url.to_string(),
            stream,
        })
    }

    /// Discover the browser websocket URL from `http://host:port/json/version`.
    /// Alias of [`super::tcp::discover_ws_url`].
    pub async fn discover(host: &str, port: u16) -> io::Result<String> {
        super::tcp::discover_ws_url(host, port).await
    }

    /// The URL this transport was created for.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl std::fmt::Debug for WsTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsTransport")
            .field("url", &self.url)
            .finish()
    }
}

fn ws_err(e: tokio_tungstenite::tungstenite::Error) -> io::Error {
    use tokio_tungstenite::tungstenite::Error as E;
    match e {
        E::Io(e) => e,
        E::ConnectionClosed | E::AlreadyClosed => {
            io::Error::new(io::ErrorKind::ConnectionAborted, e.to_string())
        }
        other => io::Error::other(other.to_string()),
    }
}

impl Transport for WsTransport {
    fn send(&mut self, frame: &[u8]) -> BoxFuture<'_, io::Result<()>> {
        let text = match std::str::from_utf8(frame) {
            Ok(s) => s.to_owned(),
            Err(e) => {
                return Box::pin(async move {
                    Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
                })
            }
        };
        Box::pin(async move {
            self.stream
                .send(Message::Text(text.into()))
                .await
                .map_err(ws_err)
        })
    }

    /// Cancel-safe: `StreamExt::next` on the websocket stream is cancel-safe.
    fn recv(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>> {
        Box::pin(async move {
            loop {
                match self.stream.next().await {
                    None => return Ok(None),
                    Some(Err(e)) => {
                        use tokio_tungstenite::tungstenite::error::ProtocolError;
                        use tokio_tungstenite::tungstenite::Error as E;
                        return match e {
                            // A peer that vanishes without the closing
                            // handshake is still just "gone" for CDP.
                            E::ConnectionClosed
                            | E::AlreadyClosed
                            | E::Protocol(ProtocolError::ResetWithoutClosingHandshake) => Ok(None),
                            other => Err(ws_err(other)),
                        };
                    }
                    Some(Ok(Message::Text(t))) => return Ok(Some(t.as_bytes().to_vec())),
                    Some(Ok(Message::Binary(b))) => return Ok(Some(b.to_vec())),
                    Some(Ok(Message::Close(_))) => return Ok(None),
                    // Pings are answered by tungstenite on the next poll; pongs and
                    // raw frames carry nothing for us.
                    Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => continue,
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// In-process echo server: proves text framing, large messages, ping
    /// handling and close → `Ok(None)`.
    #[tokio::test]
    async fn roundtrip_against_echo_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(sock).await.unwrap();
            // A ping the client must swallow silently.
            ws.send(Message::Ping(vec![1, 2, 3].into())).await.unwrap();
            // Keep reading until the stream ends so the closing handshake
            // tungstenite queues on Close is actually flushed.
            while let Some(Ok(msg)) = ws.next().await {
                if let Message::Text(t) = msg {
                    ws.send(Message::Text(t)).await.unwrap();
                }
            }
        });
        let mut t = WsTransport::connect(&format!("ws://127.0.0.1:{port}/devtools/browser/x"))
            .await
            .unwrap();
        assert!(t.url().ends_with("/devtools/browser/x"));
        t.send(br#"{"id":1,"method":"Browser.getVersion"}"#)
            .await
            .unwrap();
        assert_eq!(
            t.recv().await.unwrap().unwrap(),
            br#"{"id":1,"method":"Browser.getVersion"}"#
        );
        // Larger than tungstenite's default frame limit would allow in one
        // message? No — but large enough to span several TCP segments.
        let big = format!(r#"{{"id":2,"blob":"{}"}}"#, "x".repeat(2 * 1024 * 1024));
        t.send(big.as_bytes()).await.unwrap();
        assert_eq!(t.recv().await.unwrap().unwrap(), big.as_bytes());
        // Non-UTF-8 is refused before hitting the wire.
        assert!(t.send(&[0xff, 0xfe]).await.is_err());
        // Server closes: clean EOF.
        t.stream.close(None).await.unwrap();
        assert!(t.recv().await.unwrap().is_none());
    }
}
