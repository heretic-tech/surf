//! The [`Browser`] handle.

use crate::error::BrowserError;
use crate::launch::LaunchOptions;
use crate::page::Page;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use surf_cdp::{Connection, Session};

/// One OS browser process (or one remote connection).
pub struct Process {
    /// Root (browser) session.
    pub root: Session,
    /// The connection.
    pub conn: Arc<Connection>,
}

/// A logical browser: owns one or more [`Process`]es, hands out [`Page`]s,
/// and handles graceful shutdown. Implemented in tasks 3 and 4.
pub struct Browser {
    opts: LaunchOptions,
    processes: RefCell<Vec<Process>>,
    pages: RefCell<Vec<Page>>,
}

impl Browser {
    /// Launch (or attach, per `opts.cdp`) and return the handle.
    pub async fn launch(opts: LaunchOptions) -> Result<Rc<Browser>, BrowserError> {
        if opts.engine == "apostate" {
            return Err(BrowserError::Unsupported(
                "engine: apostate is not available yet — use engine: chrome".into(),
            ));
        }
        Ok(Rc::new(Browser {
            opts,
            processes: RefCell::new(Vec::new()),
            pages: RefCell::new(Vec::new()),
        }))
    }

    /// Options this browser was launched with.
    pub fn options(&self) -> &LaunchOptions {
        &self.opts
    }

    /// Create a new page (tab). `Target.createTarget{url:"about:blank"}` →
    /// attach → `Page.enable` → create isolated world.
    pub async fn new_page(self: &Rc<Self>) -> Result<Page, BrowserError> {
        let _ = self.processes.borrow().len();
        Err(BrowserError::Unsupported(
            "new_page not implemented yet (task 4)".into(),
        ))
    }

    /// All pages in creation order.
    pub fn pages(&self) -> Vec<Page> {
        self.pages.borrow().clone()
    }

    /// Rotate to the next proxy in `proxies`: launch a fresh process with
    /// the new proxy and [`Page::rebind`] every page onto it.
    pub async fn shift_proxy(self: &Rc<Self>) -> Result<(), BrowserError> {
        Err(BrowserError::Unsupported(
            "shift_proxy not implemented yet (task 8)".into(),
        ))
    }

    /// Graceful shutdown: `Browser.close`, wait up to a grace period, then
    /// kill; delete the temp profile unless `profile:` was set.
    pub async fn close(&self) -> Result<(), BrowserError> {
        self.processes.borrow_mut().clear();
        self.pages.borrow_mut().clear();
        Ok(())
    }
}
