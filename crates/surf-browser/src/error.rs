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
    /// An action timed out waiting for its element / condition.
    #[error("{}", timeout_message(action, selector.as_deref(), *waited_ms, last_state.as_deref()))]
    Timeout {
        /// The action (`click`, `wait_text`, `goto`, …).
        action: String,
        /// The selector involved, if any.
        selector: Option<String>,
        /// How long was waited, in milliseconds.
        waited_ms: u64,
        /// What the element looked like on the last poll (`not found`,
        /// `hidden`, `disabled`, `moving`), if known.
        last_state: Option<String>,
    },
    /// JavaScript threw inside an `eval` / helper.
    #[error("{}", script_message(text, *line))]
    Script {
        /// Exception text (`Uncaught TypeError: …`).
        text: String,
        /// 0-based line of the throw site within the evaluated source, if
        /// Chrome reported one.
        line: Option<i64>,
    },
    /// Navigation failed (`net::ERR_*`).
    #[error("navigation to {url} failed: {reason}")]
    Navigation {
        /// Target URL.
        url: String,
        /// Chrome's reason string.
        reason: String,
    },
    /// A bare action was used while several pages are open.
    #[error("several pages are open ({}) — say which: page(2).…", names.join(", "))]
    Ambiguous {
        /// Pages in creation order: `1`, `2`, `"login"`.
        names: Vec<String>,
    },
    /// The page (or its target) is closed.
    #[error("page {index} is closed")]
    PageClosed {
        /// Creation index.
        index: usize,
    },
    /// Feature that is reserved but not available.
    #[error("{0}")]
    Unsupported(String),
    /// Chrome only implements this in headless mode (`Page.printToPDF`).
    #[error("{action}() needs a headless browser — Chrome refuses it in a visible window; set `headless: true` in `browser:` (or run without a display)")]
    HeadlessOnly {
        /// The action (`pdf`).
        action: String,
    },
    /// IO.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

impl BrowserError {
    /// Whether this is a CDP protocol error whose message contains `needle`
    /// (used to spot destroyed execution contexts and closed targets).
    pub fn protocol_message_contains(&self, needle: &str) -> bool {
        matches!(self, BrowserError::Cdp(surf_cdp::CdpError::Protocol { message, .. }) if message.contains(needle))
    }
}

fn timeout_message(
    action: &str,
    selector: Option<&str>,
    waited_ms: u64,
    last_state: Option<&str>,
) -> String {
    let secs = waited_ms as f64 / 1000.0;
    let mut s = match selector {
        Some(sel) => format!("{action}({sel:?}): timed out after {secs:.1}s"),
        None => format!("{action}: timed out after {secs:.1}s"),
    };
    if let Some(state) = last_state {
        s.push_str(" (");
        s.push_str(state);
        s.push(')');
    }
    s
}

fn script_message(text: &str, line: Option<i64>) -> String {
    match line {
        Some(l) => format!("script error at line {}: {text}", l + 1),
        None => format!("script error: {text}"),
    }
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
