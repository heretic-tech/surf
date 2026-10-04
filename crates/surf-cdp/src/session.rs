//! Sessions: flat (browser) or target-attached.

use crate::connection::Connection;
use crate::error::CdpError;
use crate::event::Event;
use crate::protocol::Command;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// A handle for sending commands and receiving events on one `sessionId`
/// (or the root browser session when `session_id` is `None`). Cheap to
/// clone; clones share the connection and the per-session event channels.
#[derive(Clone)]
pub struct Session {
    conn: Arc<Connection>,
    session_id: Option<String>,
    timeout: Option<Duration>,
}

impl Session {
    pub(crate) fn new(conn: Arc<Connection>, session_id: Option<String>) -> Self {
        Self {
            conn,
            session_id,
            timeout: None,
        }
    }

    /// A copy of this handle whose calls fail with [`CdpError::Timeout`]
    /// after `timeout` (`None` = wait forever, the default).
    pub fn with_timeout(&self, timeout: Option<Duration>) -> Session {
        Session {
            conn: self.conn.clone(),
            session_id: self.session_id.clone(),
            timeout,
        }
    }

    /// The per-call timeout of this handle.
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Typed call: serialise `params`, deserialise the `result` straight
    /// from the raw response text (no intermediate `Value`).
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, CdpError> {
        let raw = self.request(method, serde_json::to_value(params)?).await?;
        Ok(serde_json::from_str(raw.get())?)
    }

    /// Untyped call. Refuses `Runtime.enable` / `DOM.enable` in every build
    /// ([`crate::FORBIDDEN_METHODS`]).
    pub async fn call_raw(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CdpError> {
        let raw = self.request(method, params).await?;
        Ok(serde_json::from_str(raw.get())?)
    }

    /// Typed call via a generated [`Command`] struct.
    pub async fn send<C: Command>(&self, c: C) -> Result<C::Response, CdpError> {
        self.call(C::METHOD, c).await
    }

    async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<Box<serde_json::value::RawValue>, CdpError> {
        let fut = self
            .conn
            .request(self.session_id.as_deref(), method, params);
        match self.timeout {
            None => fut.await,
            Some(d) => match tokio::time::timeout(d, fut).await {
                Ok(r) => r,
                Err(_) => Err(CdpError::Timeout {
                    method: method.to_owned(),
                }),
            },
        }
    }

    /// Subscribe to `Domain.event` on this session (`"*"` for every event
    /// of the session). Channels hold
    /// [`EVENT_CHANNEL_CAPACITY`](crate::connection::EVENT_CHANNEL_CAPACITY)
    /// events; a lagging receiver loses the oldest. Use
    /// [`crate::event::next`] to drain with lag logging.
    pub fn events(&self, method: &str) -> broadcast::Receiver<Event> {
        self.conn.subscribe(self.session_id.as_deref(), method)
    }

    /// Ref-counted `Domain.enable`. The first guard for a
    /// `(session, domain)` sends `Domain.enable`; dropping the last sends
    /// `Domain.disable` (spawned, best effort). Only `Page` is enabled by
    /// default (by `surf-browser`); `Network` / `Fetch` only while a hook
    /// needs them. `Runtime` and `DOM` are rejected.
    pub async fn enable_domain(&self, domain: &str) -> Result<DomainGuard, CdpError> {
        if matches!(domain, "Runtime" | "DOM") {
            return Err(CdpError::Protocol {
                code: -1,
                message: format!("quiet rule: {domain}.enable is never sent"),
                method: format!("{domain}.enable"),
            });
        }
        let key = (self.session_id.clone(), domain.to_owned());
        let _serial = self.conn.domain_lock.lock().await;
        let live = self
            .conn
            .domain_counts
            .lock()
            .expect("domain_counts poisoned")
            .get(&key)
            .copied()
            .unwrap_or(0);
        if live == 0 {
            self.call_raw(&format!("{domain}.enable"), serde_json::json!({}))
                .await?;
        }
        *self
            .conn
            .domain_counts
            .lock()
            .expect("domain_counts poisoned")
            .entry(key)
            .or_insert(0) += 1;
        Ok(DomainGuard {
            session: self.clone(),
            domain: domain.to_owned(),
        })
    }

    /// Number of live [`DomainGuard`]s for `domain` on this session.
    pub fn domain_refcount(&self, domain: &str) -> usize {
        self.conn
            .domain_counts
            .lock()
            .expect("domain_counts poisoned")
            .get(&(self.session_id.clone(), domain.to_owned()))
            .copied()
            .unwrap_or(0)
    }

    /// The `sessionId`, or `None` for the root session.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// `Target.detachFromTarget` for this session, then drop its event
    /// channels. A no-op on the root session.
    pub async fn detach(&self) -> Result<(), CdpError> {
        let Some(sid) = self.session_id.as_deref() else {
            return Ok(());
        };
        let root = Session::new(self.conn.clone(), None).with_timeout(self.timeout);
        let result = root
            .call_raw(
                "Target.detachFromTarget",
                serde_json::json!({ "sessionId": sid }),
            )
            .await;
        self.conn.forget_session(sid);
        result.map(|_| ())
    }

    /// The underlying connection.
    pub fn connection(&self) -> &Arc<Connection> {
        &self.conn
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("session_id", &self.session_id)
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Keeps a domain enabled while alive; see [`Session::enable_domain`].
pub struct DomainGuard {
    session: Session,
    domain: String,
}

impl DomainGuard {
    /// The domain this guard holds.
    pub fn domain(&self) -> &str {
        &self.domain
    }

    /// The session this guard belongs to.
    pub fn session(&self) -> &Session {
        &self.session
    }
}

impl std::fmt::Debug for DomainGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DomainGuard")
            .field("domain", &self.domain)
            .field("session_id", &self.session.session_id)
            .finish()
    }
}

impl Drop for DomainGuard {
    fn drop(&mut self) {
        let conn = self.session.conn.clone();
        let key = (self.session.session_id.clone(), self.domain.clone());
        let remaining = {
            let mut counts = conn.domain_counts.lock().expect("domain_counts poisoned");
            match counts.get_mut(&key) {
                Some(n) if *n > 1 => {
                    *n -= 1;
                    *n
                }
                Some(_) => {
                    counts.remove(&key);
                    0
                }
                None => 0,
            }
        };
        if remaining > 0 || conn.is_closed() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let session = self.session.clone();
        let domain = std::mem::take(&mut self.domain);
        handle.spawn(async move {
            let _serial = session.conn.domain_lock.lock().await;
            // Re-check: a new guard may have been taken meanwhile.
            if session.domain_refcount(&domain) > 0 {
                return;
            }
            if let Err(e) = session
                .call_raw(&format!("{domain}.disable"), serde_json::json!({}))
                .await
            {
                tracing::debug!("{domain}.disable failed (best effort): {e}");
            }
        });
    }
}
