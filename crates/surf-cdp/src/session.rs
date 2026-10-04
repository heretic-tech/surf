//! Sessions: flat (browser) or target-attached.

use crate::connection::Connection;
use crate::error::CdpError;
use crate::event::Event;
use crate::protocol::Command;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::Arc;
use tokio::sync::broadcast;

/// A handle for sending commands and receiving events on one `sessionId`
/// (or the root browser session when `session_id` is `None`). Cheap to
/// clone. Implemented in task 2.
#[derive(Clone)]
pub struct Session {
    conn: Arc<Connection>,
    session_id: Option<String>,
}

impl Session {
    pub(crate) fn new(conn: Arc<Connection>, session_id: Option<String>) -> Self {
        Self { conn, session_id }
    }

    /// Typed call: serialise `params`, deserialise the `result`.
    pub async fn call<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, CdpError> {
        let value = self.call_raw(method, serde_json::to_value(params)?).await?;
        Ok(serde_json::from_value(value)?)
    }

    /// Untyped call.
    pub async fn call_raw(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CdpError> {
        debug_assert!(
            !crate::FORBIDDEN_METHODS.contains(&method),
            "quiet rule violation: {method} must never be sent"
        );
        let _ = params;
        if self.conn.is_closed() {
            return Err(CdpError::Closed);
        }
        Err(CdpError::Protocol {
            code: -1,
            message: "session not implemented yet (task 2)".into(),
            method: method.to_string(),
        })
    }

    /// Typed call via a generated [`Command`] struct.
    pub async fn send<C: Command>(&self, c: C) -> Result<C::Response, CdpError> {
        self.call(C::METHOD, c).await
    }

    /// Subscribe to `Domain.event` on this session. Lagging receivers drop
    /// the oldest events (`broadcast` semantics).
    pub fn events(&self, method: &str) -> broadcast::Receiver<Event> {
        let _ = method;
        let (_tx, rx) = broadcast::channel(1);
        rx
    }

    /// Ref-counted `Domain.enable`. The domain is disabled when the last
    /// guard drops. Only `Page` is enabled by default (by `surf-browser`);
    /// `Network` / `Fetch` only while a hook needs them. `Runtime` and `DOM`
    /// are rejected.
    pub async fn enable_domain(&self, domain: &str) -> Result<DomainGuard, CdpError> {
        if matches!(domain, "Runtime" | "DOM") {
            return Err(CdpError::Protocol {
                code: -1,
                message: format!("quiet rule: {domain}.enable is never sent"),
                method: format!("{domain}.enable"),
            });
        }
        Ok(DomainGuard {
            session: self.clone(),
            domain: domain.to_string(),
        })
    }

    /// The `sessionId`, or `None` for the root session.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// `Target.detachFromTarget` for this session.
    pub async fn detach(&self) -> Result<(), CdpError> {
        Ok(())
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
