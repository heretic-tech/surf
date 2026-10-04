//! CDP events.

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
