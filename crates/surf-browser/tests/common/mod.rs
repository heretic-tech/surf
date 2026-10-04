//! Shared test support: the in-process fixture server (from the
//! `surf-testserver` crate, which also serves the `surf` e2e suite) and a
//! helper that launches a headless Chrome (or prints a skip message).

#![allow(dead_code)]

use std::rc::Rc;
use std::time::Duration;
use surf_browser::discovery::chrome_or_skip;
use surf_browser::{Browser, LaunchOptions};

pub use surf_testserver::Fixture;

/// Launch a headless browser for `test`, or `None` (with a printed skip
/// message) when no Chrome is available.
pub async fn browser(test: &str) -> Option<Rc<Browser>> {
    let chrome = chrome_or_skip(test)?;
    let opts = LaunchOptions {
        path: Some(chrome),
        headless: Some(true),
        timeout: Duration::from_secs(10),
        ..Default::default()
    };
    Some(Browser::launch(opts).await.expect("launch"))
}
