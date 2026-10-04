//! CDP events.

use crate::protocol::ProtocolEvent;
use tokio::sync::broadcast;

/// A CDP event as delivered by Chrome.
#[derive(Debug, Clone)]
pub struct Event {
    /// `Domain.event` name.
    pub method: String,
    /// Event payload.
    pub params: serde_json::Value,
    /// Session the event belongs to; `None` for the browser (root) session.
    pub session_id: Option<String>,
}

impl Event {
    /// Deserialise `params` into a generated event struct. Errors if the
    /// method does not match `E::METHOD` or the payload does not fit.
    pub fn parse<E: ProtocolEvent>(&self) -> Result<E, serde_json::Error> {
        if self.method != E::METHOD {
            return Err(serde::de::Error::custom(format!(
                "event is {} not {}",
                self.method,
                E::METHOD
            )));
        }
        serde_json::from_value(self.params.clone())
    }
}

/// Next event from a [`Session::events`](crate::Session::events) receiver.
/// Skips over lag (logging a `warn!` with the number of lost events) and
/// returns `None` once the channel is closed (session detached or
/// connection gone).
pub async fn next(rx: &mut broadcast::Receiver<Event>) -> Option<Event> {
    loop {
        match rx.recv().await {
            Ok(ev) => return Some(ev),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!("CDP event subscriber lagged: {n} events dropped");
            }
            Err(broadcast::error::RecvError::Closed) => return None,
        }
    }
}
