//! Error type.

use thiserror::Error;

/// Browser-level failures. Converted to `surf_vm::RuntimeError` (with
/// `cdp_method` / `selector` filled in) by `surf-runtime`.
#[derive(Debug, Error)]
pub enum BrowserError {
    /// No usable Chrome/Chromium was found. `tried` lists every location
    /// discovery looked at, in order.
    #[error("{}", not_found_message(tried))]
    NotFound {
        /// Locations checked, in order.
        tried: Vec<String>,
    },
    /// Chrome failed to start or died early.
    #[error("browser failed to launch: {0}")]
    Launch(String),
    /// A `browser:` value could not be understood (`proxy`, `size`, …).
    #[error("invalid {what}: {reason}")]
    Config {
        /// Which option.
        what: String,
        /// Why.
        reason: String,
    },
    /// A CDP error.
    #[error(transparent)]
    Cdp(#[from] surf_cdp::CdpError),
    /// Element lookup / auto-wait failure.
    #[error("{message} (selector: {selector})")]
    Element {
        /// What went wrong.
        message: String,
        /// The selector involved.
        selector: String,
    },
    /// Timed out waiting for a condition.
    #[error("timed out after {after_ms} ms: {what}")]
    Timeout {
        /// Description of what was awaited.
        what: String,
        /// Timeout in milliseconds.
        after_ms: u64,
    },
    /// Navigation failed (`net::ERR_*`).
    #[error("navigation to {url} failed: {reason}")]
    Navigation {
        /// Target URL.
        url: String,
        /// Chrome's reason string.
        reason: String,
    },
    /// Feature that is reserved but not available.
    #[error("{0}")]
    Unsupported(String),
    /// IO.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

fn not_found_message(tried: &[String]) -> String {
    let mut s = String::from(
        "no Chrome/Chromium found; set SURF_CHROME=/path/to/chrome or `browser:\n    path: \"…\"`",
    );
    if !tried.is_empty() {
        s.push_str("\nlooked in:");
        for t in tried {
            s.push_str("\n  ");
            s.push_str(t);
        }
    }
    s
}
