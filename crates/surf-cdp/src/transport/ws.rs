//! WebSocket transport — only for `cdp: 9222`, `cdp: "ws://…"` and
//! `pool: "wss://…"`. Never the default. Implemented in task 2.

use super::Transport;
use futures::future::BoxFuture;
use std::io;

/// A `tokio-tungstenite` backed transport.
pub struct WebSocketTransport {
    url: String,
}

impl WebSocketTransport {
    /// Connect to a DevTools websocket URL (`ws://host:port/devtools/browser/<id>`).
    pub async fn connect(url: &str) -> io::Result<Self> {
        Ok(Self {
            url: url.to_string(),
        })
    }

    /// Discover the browser websocket URL from an `http://host:port/json/version`
    /// endpoint.
    pub async fn discover(host: &str, port: u16) -> io::Result<String> {
        Err(io::Error::other(format!(
            "websocket discovery not implemented yet (task 2): {host}:{port}"
        )))
    }

    /// The URL this transport was created for.
    pub fn url(&self) -> &str {
        &self.url
    }
}

impl Transport for WebSocketTransport {
    fn send(&mut self, frame: &[u8]) -> BoxFuture<'_, io::Result<()>> {
        let _ = frame;
        Box::pin(async {
            Err(io::Error::other(
                "websocket transport not implemented yet (task 2)",
            ))
        })
    }

    fn recv(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>> {
        Box::pin(async {
            Err(io::Error::other(
                "websocket transport not implemented yet (task 2)",
            ))
        })
    }
}
