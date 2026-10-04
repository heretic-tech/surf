//! Auto-waiting page actions.
//!
//! Every action waits (attached → visible → stable → enabled) up to the
//! page's `timeout` before acting, polling the isolated-world resolver.
//! Implemented in task 4.

use crate::error::BrowserError;
use crate::page::Page;
use crate::selector::Selector;
use std::time::Duration;

/// Per-call options shared by most actions.
#[derive(Debug, Clone, Default)]
pub struct ActionOptions {
    /// Override the page timeout.
    pub timeout: Option<Duration>,
}

/// Navigate and wait for `Page.loadEventFired` (or `wait_until`).
pub async fn goto(page: &Page, url: &str, opts: ActionOptions) -> Result<(), BrowserError> {
    let _ = (page.index(), url, opts);
    Err(BrowserError::Unsupported(
        "goto not implemented yet (task 4)".into(),
    ))
}

/// Click the first match of `selector`.
pub async fn click(
    page: &Page,
    selector: &Selector,
    opts: ActionOptions,
) -> Result<(), BrowserError> {
    let _ = (page.index(), selector, opts);
    Err(BrowserError::Unsupported(
        "click not implemented yet (task 4)".into(),
    ))
}

/// Type text key-by-key into the first match.
pub async fn type_text(
    page: &Page,
    selector: &Selector,
    text: &str,
    opts: ActionOptions,
) -> Result<(), BrowserError> {
    let _ = (page.index(), selector, text, opts);
    Err(BrowserError::Unsupported(
        "type not implemented yet (task 4)".into(),
    ))
}

/// Inner text of the first match.
pub async fn text(
    page: &Page,
    selector: &Selector,
    opts: ActionOptions,
) -> Result<String, BrowserError> {
    let _ = (page.index(), selector, opts);
    Err(BrowserError::Unsupported(
        "text not implemented yet (task 4)".into(),
    ))
}
