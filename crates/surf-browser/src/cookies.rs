//! Cookies via `Storage.getCookies` / `Storage.setCookies` (browser-context
//! scoped; no `Network.enable` needed). Implemented in task 9.

use serde::{Deserialize, Serialize};

/// A cookie as exchanged with CDP.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Cookie {
    /// Name.
    pub name: String,
    /// Value.
    pub value: String,
    /// Domain.
    #[serde(default)]
    pub domain: Option<String>,
    /// Path.
    #[serde(default)]
    pub path: Option<String>,
    /// Expiry (unix seconds); `-1` for session cookies.
    #[serde(default)]
    pub expires: Option<f64>,
    /// `HttpOnly`.
    #[serde(default)]
    pub http_only: Option<bool>,
    /// `Secure`.
    #[serde(default)]
    pub secure: Option<bool>,
    /// `SameSite`.
    #[serde(default)]
    pub same_site: Option<String>,
}
