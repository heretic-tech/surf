//! Lazy browser slots: a `browser:` block (or the implicit default) is a
//! [`BrowserSlot`] holding a [`BrowserConfig`]; the process is launched (or
//! the websocket attached) the first time an action needs a page, and
//! every later request on the slot returns the same [`Browser`].

use crate::config::BrowserConfig;
use crate::errors::convert;
use crate::host::Runtime;
use std::cell::RefCell;
use std::rc::Rc;
use surf_browser::Browser;
use surf_vm::RuntimeError;
use tokio::sync::Mutex;

/// Alias of the unnamed `browser:` block / the implicit default browser.
pub const DEFAULT_ALIAS: &str = "default";

/// One declared (or implicit) browser.
pub struct BrowserSlot {
    /// `"default"` or the name after `browser`.
    pub alias: String,
    /// Configuration; overrides from the CLI are applied at launch.
    pub config: RefCell<BrowserConfig>,
    handle: RefCell<Option<Rc<Browser>>>,
    /// Serialises concurrent first uses so only one launch happens.
    launching: Mutex<()>,
}

impl std::fmt::Debug for BrowserSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserSlot")
            .field("alias", &self.alias)
            .field("live", &self.is_live())
            .finish()
    }
}

impl BrowserSlot {
    /// A slot that has not launched anything yet.
    pub fn new(alias: &str, config: BrowserConfig) -> Rc<BrowserSlot> {
        Rc::new(BrowserSlot {
            alias: alias.to_owned(),
            config: RefCell::new(config),
            handle: RefCell::new(None),
            launching: Mutex::new(()),
        })
    }

    /// The launched browser, if any.
    pub fn browser(&self) -> Option<Rc<Browser>> {
        self.handle.borrow().clone()
    }

    /// Whether a process / connection exists.
    pub fn is_live(&self) -> bool {
        self.handle.borrow().is_some()
    }

    /// Launch (or attach) if needed and return the browser.
    pub async fn get(&self, rt: &Runtime) -> Result<Rc<Browser>, RuntimeError> {
        if let Some(b) = self.browser() {
            return Ok(b);
        }
        let _guard = self.launching.lock().await;
        if let Some(b) = self.browser() {
            return Ok(b);
        }
        let config = {
            let mut c = self.config.borrow_mut();
            c.apply_overrides(rt.options());
            c.clone()
        };
        config.check(&self.alias)?;
        let launch = config.launch.clone();
        if launch.headless.is_none() && launch.headless_decision() {
            rt.notice_headless();
        }
        let action = if self.alias == DEFAULT_ALIAS {
            "launching the browser".to_string()
        } else {
            format!("launching browser {}", self.alias)
        };
        let browser = match &config.pool {
            Some(url) => Browser::connect(url, launch).await,
            None => Browser::launch(launch).await,
        }
        .map_err(|e| convert(e, &action))?;
        if rt.options().trace_cdp {
            browser.connection().set_trace(true);
        }
        *self.handle.borrow_mut() = Some(browser.clone());
        Ok(browser)
    }

    /// Close the browser if it was launched (idempotent).
    pub async fn close(&self) {
        let b = self.handle.borrow_mut().take();
        if let Some(b) = b {
            if let Err(e) = b.close().await {
                tracing::debug!("closing browser {}: {e}", self.alias);
            }
        }
    }
}

/// `several browsers are declared ("work", "home") — say which: work.goto(…)`.
pub fn several_browsers_message(aliases: &[String], action: &str) -> String {
    let names: Vec<String> = aliases.iter().map(|a| format!("{a:?}")).collect();
    let first = aliases.first().map(String::as_str).unwrap_or("work");
    format!(
        "several browsers are declared ({}) — say which: {first}.{action}(…)",
        names.join(", ")
    )
}
