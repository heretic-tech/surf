//! Request/response multiplexing over one transport.

use crate::error::CdpError;
use crate::session::Session;
use crate::transport::Transport;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

/// One CDP connection to one browser process (or remote provider).
///
/// Owns the transport writer and a reader task that routes responses to
/// waiting callers (by `id`) and events to per-session broadcast channels
/// (by `sessionId` + `method`). Implemented in task 2.
pub struct Connection {
    transport: Mutex<Box<dyn Transport>>,
    next_id: AtomicU64,
    closed: AtomicBool,
    trace: AtomicBool,
}

impl Connection {
    /// Wrap a transport and start the reader task.
    pub fn new(t: Box<dyn Transport>) -> Arc<Connection> {
        Arc::new(Connection {
            transport: Mutex::new(t),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
            trace: AtomicBool::new(false),
        })
    }

    /// The browser-level (flat, no `sessionId`) session.
    pub fn root(self: &Arc<Self>) -> Session {
        Session::new(self.clone(), None)
    }

    /// `Target.attachToTarget{targetId, flatten:true}` → session.
    pub async fn attach(self: &Arc<Self>, target_id: &str) -> Result<Session, CdpError> {
        let _ = self.next_id.load(Ordering::Relaxed);
        let _ = self.transport.lock().await;
        Err(CdpError::Protocol {
            code: -1,
            message: format!("attach not implemented yet (task 2): {target_id}"),
            method: "Target.attachToTarget".into(),
        })
    }

    /// Log every frame in both directions at `tracing::trace!` level
    /// (`surf run --trace-cdp`).
    pub fn set_trace(&self, on: bool) {
        self.trace.store(on, Ordering::Relaxed);
    }

    /// Whether the transport has closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Whether frame tracing is on.
    pub fn trace_enabled(&self) -> bool {
        self.trace.load(Ordering::Relaxed)
    }
}
