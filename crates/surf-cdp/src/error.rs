//! Error type.

use thiserror::Error;

/// Anything that can go wrong talking to Chrome.
#[derive(Debug, Error)]
pub enum CdpError {
    /// Chrome returned an error object for a command.
    #[error("{method}: {message} (code {code})")]
    Protocol {
        /// JSON-RPC error code.
        code: i64,
        /// Error message from Chrome.
        message: String,
        /// The method that failed.
        method: String,
    },
    /// The connection was closed (transport EOF / detach).
    #[error("CDP connection closed")]
    Closed,
    /// The browser process exited unexpectedly.
    #[error("browser crashed (exit code {exit_code:?}): {stderr_tail}")]
    BrowserCrashed {
        /// Process exit code, if known.
        exit_code: Option<i32>,
        /// Last lines of the browser's stderr.
        stderr_tail: String,
    },
    /// Transport IO error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON (de)serialisation error.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// A command did not answer in time.
    #[error("{method}: timed out")]
    Timeout {
        /// The method that timed out.
        method: String,
    },
}
