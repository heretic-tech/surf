//! Channel-backed transport for unit tests: the test plays the browser.

use super::Transport;
use futures::future::BoxFuture;
use std::io;
use tokio::sync::mpsc;

/// The "browser" side of a [`FakeTransport`].
pub struct FakeBrowser {
    /// Frames the connection sent (what Chrome would read from fd 3).
    pub sent: mpsc::UnboundedReceiver<Vec<u8>>,
    /// Frames to deliver to the connection (what Chrome would write to fd 4).
    pub inject: mpsc::UnboundedSender<Vec<u8>>,
}

impl FakeBrowser {
    /// Next frame the connection sent, parsed.
    pub async fn next_sent(&mut self) -> serde_json::Value {
        let frame = self.sent.recv().await.expect("connection closed");
        serde_json::from_slice(&frame).expect("connection sent invalid JSON")
    }

    /// Next frame, or `None` if nothing arrives within `ms` milliseconds.
    pub async fn next_sent_within(&mut self, ms: u64) -> Option<serde_json::Value> {
        tokio::time::timeout(std::time::Duration::from_millis(ms), self.next_sent())
            .await
            .ok()
    }

    /// Deliver a frame to the connection.
    pub fn push(&self, v: serde_json::Value) {
        self.inject
            .send(serde_json::to_vec(&v).unwrap())
            .expect("connection gone");
    }

    /// Deliver raw bytes (for malformed-frame tests).
    pub fn push_raw(&self, bytes: &[u8]) {
        self.inject.send(bytes.to_vec()).expect("connection gone");
    }

    /// Answer request `id` with `result`.
    pub fn respond(&self, id: u64, result: serde_json::Value) {
        self.push(serde_json::json!({ "id": id, "result": result }));
    }

    /// Answer request `id` with a protocol error.
    pub fn fail(&self, id: u64, code: i64, message: &str) {
        self.push(serde_json::json!({ "id": id, "error": { "code": code, "message": message } }));
    }

    /// Emit an event.
    pub fn event(&self, method: &str, params: serde_json::Value, session_id: Option<&str>) {
        let mut v = serde_json::json!({ "method": method, "params": params });
        if let Some(s) = session_id {
            v["sessionId"] = serde_json::Value::String(s.to_owned());
        }
        self.push(v);
    }

    /// Simulate the browser closing the pipe.
    pub fn hang_up(self) {
        drop(self.inject);
    }
}

/// The connection side.
pub struct FakeTransport {
    incoming: mpsc::UnboundedReceiver<Vec<u8>>,
    outgoing: mpsc::UnboundedSender<Vec<u8>>,
}

/// A connected `(transport, browser)` pair.
pub fn pair() -> (FakeTransport, FakeBrowser) {
    let (inject, incoming) = mpsc::unbounded_channel();
    let (outgoing, sent) = mpsc::unbounded_channel();
    (
        FakeTransport { incoming, outgoing },
        FakeBrowser { sent, inject },
    )
}

impl Transport for FakeTransport {
    fn send(&mut self, frame: &[u8]) -> BoxFuture<'_, io::Result<()>> {
        let r = self
            .outgoing
            .send(frame.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "fake browser gone"));
        Box::pin(async move { r })
    }

    fn recv(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>> {
        Box::pin(async move { Ok(self.incoming.recv().await) })
    }
}
