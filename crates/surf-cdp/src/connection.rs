//! Request/response multiplexing over one transport.
//!
//! One driver task owns the transport. It `select!`s between the outgoing
//! queue (frames from [`Session::call`](crate::Session::call)) and
//! `Transport::recv`; every incoming frame is parsed once and routed:
//!
//! * `{"id": n, "result" | "error"}` → the `oneshot` registered for `n`
//!   (the id space is connection-wide and monotonic; `sessionId` on a
//!   response is not needed for routing);
//! * `{"method", "params", "sessionId"?}` → the `broadcast` channel for
//!   `(sessionId, method)` plus the per-session catch-all `"*"` channel.
//!
//! The driver holds only a `Weak<Connection>`: dropping the last
//! [`Session`]/`Arc<Connection>` closes the outgoing queue, which ends the
//! driver, which drops the transport (closing the pipe / websocket).

use crate::error::CdpError;
use crate::event::Event;
use crate::session::Session;
use crate::transport::Transport;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Instant;
use tokio::sync::{broadcast, mpsc, oneshot, watch};

/// Capacity of every per-method event channel. A subscriber that falls
/// more than this many events behind loses the oldest (`Lagged`); the
/// sender logs a `warn!` when that is about to happen.
pub const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// Env var that turns frame tracing on at construction (same as
/// [`Connection::set_trace`]; `surf run --trace-cdp` sets it).
pub const TRACE_ENV: &str = "SURF_TRACE_CDP";

/// Catch-all method name for [`Session::events`]: receives every event of
/// the session.
pub const ALL_EVENTS: &str = "*";

/// Frames longer than this are truncated in trace output.
const TRACE_MAX_BYTES: usize = 4096;

/// Target of the frame trace (`RUST_LOG=surf_cdp::trace=debug`).
const TRACE_TARGET: &str = "surf_cdp::trace";

/// When the process started, for the one `info` line the trace emits:
/// `first CDP frame sent N ms after start` (a cold-start measurement the
/// perf gate reads back). Set by [`mark_process_start`]; falls back to the
/// creation of the first [`Connection`].
static PROCESS_START: OnceLock<Instant> = OnceLock::new();
static FIRST_FRAME_LOGGED: AtomicBool = AtomicBool::new(false);

/// Record "now" as the process start for the first-frame trace line. Call
/// once, as early as possible in `main`; later calls are ignored.
pub fn mark_process_start() {
    let _ = PROCESS_START.set(Instant::now());
}

type RawResult = Result<Box<RawValue>, CdpError>;

struct Pending {
    method: String,
    tx: oneshot::Sender<RawResult>,
}

enum Outgoing {
    Frame(Vec<u8>),
    Close,
}

type Routes = HashMap<Option<String>, HashMap<String, broadcast::Sender<Event>>>;

/// One CDP connection to one browser process (or remote provider).
///
/// Owns the outgoing queue and the driver task that routes responses to
/// waiting callers (by `id`) and events to per-session broadcast channels
/// (by `sessionId` + `method`). `Send + Sync`; cheap to share via `Arc`.
///
/// `new` must be called from within a tokio runtime (it spawns the driver).
pub struct Connection {
    outgoing: mpsc::UnboundedSender<Outgoing>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, Pending>>,
    routes: Mutex<Routes>,
    closed: AtomicBool,
    closed_tx: watch::Sender<bool>,
    /// Set by [`mark_crashed`](Self::mark_crashed): `(exit_code, stderr_tail)`.
    crash: Mutex<Option<(Option<i32>, String)>>,
    trace: AtomicBool,
    /// Serialises `Domain.enable` / `Domain.disable` traffic.
    pub(crate) domain_lock: tokio::sync::Mutex<()>,
    /// Live [`crate::DomainGuard`] count per `(sessionId, domain)`.
    pub(crate) domain_counts: Mutex<HashMap<(Option<String>, String), usize>>,
}

impl Connection {
    /// Wrap a transport and start the driver task. Frame tracing starts on
    /// if `SURF_TRACE_CDP` is set to anything but `0`/empty.
    pub fn new(t: Box<dyn Transport>) -> Arc<Connection> {
        let (outgoing, rx) = mpsc::unbounded_channel();
        let (closed_tx, _) = watch::channel(false);
        let trace = std::env::var(TRACE_ENV)
            .map(|v| !(v.is_empty() || v == "0" || v.eq_ignore_ascii_case("false")))
            .unwrap_or(false);
        let conn = Arc::new(Connection {
            outgoing,
            next_id: AtomicU64::new(1),
            pending: Mutex::new(HashMap::new()),
            routes: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            closed_tx,
            crash: Mutex::new(None),
            trace: AtomicBool::new(trace),
            domain_lock: tokio::sync::Mutex::new(()),
            domain_counts: Mutex::new(HashMap::new()),
        });
        tokio::spawn(drive(Arc::downgrade(&conn), t, rx));
        conn
    }

    /// The browser-level (flat, no `sessionId`) session.
    pub fn root(self: &Arc<Self>) -> Session {
        Session::new(self.clone(), None)
    }

    /// A handle for a `sessionId` you already know (e.g. from a
    /// `Target.attachedToTarget` event under auto-attach).
    pub fn session(self: &Arc<Self>, session_id: impl Into<String>) -> Session {
        Session::new(self.clone(), Some(session_id.into()))
    }

    /// `Target.attachToTarget{targetId, flatten:true}` → session.
    pub async fn attach(self: &Arc<Self>, target_id: &str) -> Result<Session, CdpError> {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Attached {
            session_id: String,
        }
        let attached: Attached = self
            .root()
            .call(
                "Target.attachToTarget",
                serde_json::json!({ "targetId": target_id, "flatten": true }),
            )
            .await?;
        Ok(self.session(attached.session_id))
    }

    /// Log every frame in both directions at `tracing::debug!` level under
    /// the `surf_cdp::trace` target, `→` for sent and `←` for received
    /// (`surf run --trace-cdp`). Frames over 4 KiB are truncated.
    pub fn set_trace(&self, on: bool) {
        self.trace.store(on, Ordering::Relaxed);
    }

    /// Whether frame tracing is on.
    pub fn trace_enabled(&self) -> bool {
        self.trace.load(Ordering::Relaxed)
    }

    /// Whether the transport has closed (EOF, IO error, or [`close`](Self::close)).
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Resolves once the connection is closed.
    pub async fn wait_closed(&self) {
        let mut rx = self.closed_tx.subscribe();
        while !*rx.borrow_and_update() {
            if rx.changed().await.is_err() {
                break;
            }
        }
    }

    /// Close the transport. In-flight calls fail with [`CdpError::Closed`];
    /// event receivers see `RecvError::Closed`.
    pub fn close(&self) {
        let _ = self.outgoing.send(Outgoing::Close);
    }

    /// The browser process died unexpectedly: close the connection and make
    /// every in-flight and future call fail with
    /// [`CdpError::BrowserCrashed`] (carrying `exit_code` and the last lines
    /// of stderr) instead of the bare [`CdpError::Closed`]. Called by the
    /// launcher's process watcher; a no-op if already closed.
    pub fn mark_crashed(&self, exit_code: Option<i32>, stderr_tail: String) {
        if self.is_closed() {
            return;
        }
        *self.crash.lock().expect("crash poisoned") = Some((exit_code, stderr_tail));
        self.mark_closed();
        let _ = self.outgoing.send(Outgoing::Close);
    }

    /// The error a call gets once the connection is closed.
    fn closed_error(&self) -> CdpError {
        match &*self.crash.lock().expect("crash poisoned") {
            Some((exit_code, stderr_tail)) => CdpError::BrowserCrashed {
                exit_code: *exit_code,
                stderr_tail: stderr_tail.clone(),
            },
            None => CdpError::Closed,
        }
    }

    /// Allocate an id, queue the frame, await the matching response.
    pub(crate) async fn request(
        &self,
        session_id: Option<&str>,
        method: &str,
        params: serde_json::Value,
    ) -> RawResult {
        if crate::FORBIDDEN_METHODS.contains(&method) {
            return Err(CdpError::Protocol {
                code: -1,
                message: format!("quiet rule: {method} is never sent"),
                method: method.to_owned(),
            });
        }
        if self.is_closed() {
            return Err(self.closed_error());
        }
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Request<'a> {
            id: u64,
            method: &'a str,
            params: serde_json::Value,
            #[serde(skip_serializing_if = "Option::is_none")]
            session_id: Option<&'a str>,
        }
        let params = if params.is_null() {
            serde_json::Value::Object(Default::default())
        } else {
            params
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let frame = serde_json::to_vec(&Request {
            id,
            method,
            params,
            session_id,
        })?;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().expect("pending poisoned").insert(
            id,
            Pending {
                method: method.to_owned(),
                tx,
            },
        );
        let mut guard = PendingGuard {
            conn: self,
            id,
            armed: true,
        };
        if self.trace_enabled() {
            if !FIRST_FRAME_LOGGED.swap(true, Ordering::Relaxed) {
                let since = PROCESS_START.get_or_init(Instant::now).elapsed();
                tracing::info!(
                    target: TRACE_TARGET,
                    "first CDP frame sent {:.1} ms after start",
                    since.as_secs_f64() * 1e3
                );
            }
            tracing::debug!(target: TRACE_TARGET, "→ {}", trace_text(&frame));
        }
        if self.outgoing.send(Outgoing::Frame(frame)).is_err() {
            return Err(self.closed_error());
        }
        let result = rx.await.unwrap_or_else(|_| Err(self.closed_error()));
        guard.armed = false;
        result
    }

    /// Subscribe to `(session_id, method)`.
    pub(crate) fn subscribe(
        &self,
        session_id: Option<&str>,
        method: &str,
    ) -> broadcast::Receiver<Event> {
        if self.is_closed() {
            let (tx, rx) = broadcast::channel(1);
            drop(tx);
            return rx;
        }
        let mut routes = self.routes.lock().expect("routes poisoned");
        let per_session = routes.entry(session_id.map(str::to_owned)).or_default();
        match per_session.get(method) {
            Some(tx) => tx.subscribe(),
            None => {
                let (tx, rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
                per_session.insert(method.to_owned(), tx);
                rx
            }
        }
    }

    /// Drop every event channel of a session (after detach).
    pub(crate) fn forget_session(&self, session_id: &str) {
        self.routes
            .lock()
            .expect("routes poisoned")
            .remove(&Some(session_id.to_owned()));
    }

    /// Parse and route one incoming frame.
    fn dispatch(&self, frame: &[u8]) {
        if self.trace_enabled() {
            tracing::debug!(target: TRACE_TARGET, "← {}", trace_text(frame));
        }
        let text = match std::str::from_utf8(frame) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("dropping non-UTF-8 CDP frame: {e}");
                return;
            }
        };
        let msg: Incoming = match serde_json::from_str(text) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!("dropping unparseable CDP frame: {e}");
                return;
            }
        };
        match (msg.id, msg.method) {
            (Some(id), _) => self.complete(id, msg.result, msg.error),
            (None, Some(method)) => self.publish(method, msg.params, msg.session_id),
            (None, None) => tracing::warn!("CDP frame with neither id nor method"),
        }
    }

    fn complete(&self, id: u64, result: Option<Box<RawValue>>, error: Option<ProtocolError>) {
        let Some(pending) = self.pending.lock().expect("pending poisoned").remove(&id) else {
            tracing::debug!("response for unknown id {id} (caller gave up?)");
            return;
        };
        let outcome = match error {
            Some(e) => Err(CdpError::Protocol {
                code: e.code,
                message: match e.data {
                    Some(serde_json::Value::String(d)) if !d.is_empty() => {
                        format!("{} ({d})", e.message)
                    }
                    _ => e.message,
                },
                method: pending.method,
            }),
            None => Ok(result.unwrap_or_else(empty_object)),
        };
        // The caller may have gone away (timeout); that is fine.
        let _ = pending.tx.send(outcome);
    }

    fn publish(&self, method: String, params: Option<Box<RawValue>>, session_id: Option<String>) {
        let params = match params {
            Some(raw) => match serde_json::from_str(raw.get()) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("{method}: unparseable params: {e}");
                    return;
                }
            },
            None => serde_json::Value::Object(Default::default()),
        };
        let detached = if method == "Target.detachedFromTarget" {
            params
                .get("sessionId")
                .and_then(|s| s.as_str())
                .map(str::to_owned)
        } else {
            None
        };
        let event = Event {
            method,
            params,
            session_id,
        };
        let mut routes = self.routes.lock().expect("routes poisoned");
        if let Some(per_session) = routes.get_mut(&event.session_id) {
            for key in [event.method.as_str(), ALL_EVENTS] {
                let dead = match per_session.get(key) {
                    Some(tx) => {
                        if tx.len() >= EVENT_CHANNEL_CAPACITY {
                            tracing::warn!(
                                "event channel {}/{key} is full ({EVENT_CHANNEL_CAPACITY}); a slow subscriber will lag",
                                event.session_id.as_deref().unwrap_or("root")
                            );
                        }
                        tx.send(event.clone()).is_err()
                    }
                    None => false,
                };
                if dead {
                    // No live receivers: drop the channel so it can be
                    // re-created fresh (and stop cloning events into it).
                    per_session.remove(key);
                }
            }
        }
        if let Some(sid) = detached {
            routes.remove(&Some(sid));
        }
    }

    /// Transport gone: fail everything that is waiting.
    fn mark_closed(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let pending: Vec<_> = self
            .pending
            .lock()
            .expect("pending poisoned")
            .drain()
            .collect();
        for (_, p) in pending {
            let _ = p.tx.send(Err(self.closed_error()));
        }
        self.routes.lock().expect("routes poisoned").clear();
        // `send` would be a no-op without live receivers; `send_replace`
        // always stores so later `wait_closed` callers see it.
        self.closed_tx.send_replace(true);
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("closed", &self.is_closed())
            .field("next_id", &self.next_id.load(Ordering::Relaxed))
            .finish()
    }
}

/// Removes the pending entry if the caller is dropped before the response.
struct PendingGuard<'a> {
    conn: &'a Connection,
    id: u64,
    armed: bool,
}

impl Drop for PendingGuard<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut p) = self.conn.pending.lock() {
            p.remove(&self.id);
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Incoming {
    id: Option<u64>,
    method: Option<String>,
    params: Option<Box<RawValue>>,
    result: Option<Box<RawValue>>,
    error: Option<ProtocolError>,
    session_id: Option<String>,
}

#[derive(Deserialize)]
struct ProtocolError {
    code: i64,
    message: String,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

fn empty_object() -> Box<RawValue> {
    RawValue::from_string("{}".to_owned()).expect("static JSON")
}

/// Frame text for the trace log, truncated on a UTF-8 boundary.
fn trace_text(frame: &[u8]) -> String {
    if frame.len() <= TRACE_MAX_BYTES {
        return String::from_utf8_lossy(frame).into_owned();
    }
    // Back up until `cut` is not inside a multi-byte sequence.
    let cut = (0..=TRACE_MAX_BYTES)
        .rev()
        .find(|&i| i == 0 || (frame[i] & 0xC0) != 0x80)
        .unwrap_or(0);
    format!(
        "{}… (+{} bytes)",
        String::from_utf8_lossy(&frame[..cut]),
        frame.len() - cut
    )
}

/// The single task that owns the transport.
async fn drive(
    conn: Weak<Connection>,
    mut transport: Box<dyn Transport>,
    mut outgoing: mpsc::UnboundedReceiver<Outgoing>,
) {
    loop {
        tokio::select! {
            biased;
            out = outgoing.recv() => match out {
                Some(Outgoing::Frame(frame)) => {
                    if let Err(e) = transport.send(&frame).await {
                        tracing::warn!("CDP transport write failed: {e}");
                        break;
                    }
                }
                Some(Outgoing::Close) | None => break,
            },
            msg = transport.recv() => match msg {
                Ok(Some(frame)) => {
                    let Some(conn) = conn.upgrade() else { break };
                    conn.dispatch(&frame);
                }
                Ok(None) => {
                    tracing::debug!("CDP transport EOF");
                    break;
                }
                Err(e) => {
                    tracing::warn!("CDP transport read failed: {e}");
                    break;
                }
            },
        }
    }
    drop(transport);
    if let Some(conn) = conn.upgrade() {
        conn.mark_closed();
    }
}
