//! Implicit page resolution (`docs/language.md` § 5).
//!
//! The page registry itself lives in [`surf_browser::Browser`] (`page(n)`,
//! `page("name")`, `sole_page`). This module adds the two runtime rules on
//! top: which *browser* a bare action means, and the per-task private page
//! — inside a handler body (and, from task 8, a spawned task / actor /
//! `parallel for` item) the first bare action binds a page to that task so
//! concurrent bodies never fight over one tab.

use crate::browsers::{several_browsers_message, BrowserSlot, DEFAULT_ALIAS};
use crate::errors::convert;
use crate::host::Runtime;
use std::cell::RefCell;
use std::rc::Rc;
use surf_browser::{Browser, NewPageOptions, Page};
use surf_vm::RuntimeError;

/// What kind of task the current code runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskKind {
    /// The main body (and the REPL): bare actions use the browser's sole
    /// page.
    Main,
    /// A handler body: bound to the page the event fired on.
    Handler,
    /// A spawned task / actor / `parallel for` item: the first bare action
    /// creates a private page.
    Spawned,
}

/// Per-task state, installed with [`TASK_CTX`]`.scope(…)`.
pub struct TaskCtx {
    /// Task kind.
    pub kind: TaskKind,
    /// The page bare actions resolve to, once bound.
    pub page: RefCell<Option<(Rc<Browser>, Page)>>,
}

impl TaskCtx {
    /// A handler task bound to `page`.
    pub fn handler(browser: Rc<Browser>, page: Page) -> Rc<TaskCtx> {
        Rc::new(TaskCtx {
            kind: TaskKind::Handler,
            page: RefCell::new(Some((browser, page))),
        })
    }

    /// A spawned task with no page yet.
    pub fn spawned() -> Rc<TaskCtx> {
        Rc::new(TaskCtx {
            kind: TaskKind::Spawned,
            page: RefCell::new(None),
        })
    }
}

tokio::task_local! {
    /// The current task's context; absent in the main body.
    pub static TASK_CTX: Rc<TaskCtx>;
}

/// The task context of the running code, if any.
pub fn current_ctx() -> Option<Rc<TaskCtx>> {
    TASK_CTX.try_with(|c| c.clone()).ok()
}

impl Runtime {
    /// The browser a bare action (or `page` / `browser`) means: the
    /// unnamed `browser:` block, the implicit default when nothing is
    /// declared, the only named browser when exactly one is declared;
    /// several named browsers without an unnamed block is an error that
    /// names them.
    pub fn default_slot(&self, action: &str) -> Result<Rc<BrowserSlot>, RuntimeError> {
        if let Some(s) = self.slot(DEFAULT_ALIAS) {
            return Ok(s);
        }
        let aliases = self.aliases();
        match aliases.len() {
            0 => Ok(self.ensure_slot(DEFAULT_ALIAS)),
            1 => Ok(self.slot(&aliases[0]).expect("declared slot")),
            _ => Err(RuntimeError::new(several_browsers_message(
                &aliases, action,
            ))),
        }
    }

    /// The default browser, launched on first use.
    pub async fn default_browser(&self, action: &str) -> Result<Rc<Browser>, RuntimeError> {
        let slot = self.default_slot(action)?;
        slot.get(self).await
    }

    /// The page a bare action resolves to: the task's private page when
    /// inside a handler / spawned task, otherwise the sole page of the
    /// default browser (created when none exists; an error listing the
    /// pages when several are open).
    pub async fn sole_page(&self, action: &str) -> Result<(Rc<Browser>, Page), RuntimeError> {
        let ctx = current_ctx();
        if let Some(ctx) = &ctx {
            if let Some((b, p)) = ctx.page.borrow().clone() {
                if p.is_open() {
                    return Ok((b, p));
                }
            }
        }
        let browser = self.default_browser(action).await?;
        let page = match ctx.as_ref().map(|c| c.kind) {
            Some(TaskKind::Handler) | Some(TaskKind::Spawned) => browser
                .new_page(NewPageOptions::default())
                .await
                .map_err(|e| convert(e, action))?,
            _ => browser.sole_page().await.map_err(|e| convert(e, action))?,
        };
        if let Some(ctx) = &ctx {
            *ctx.page.borrow_mut() = Some((browser.clone(), page.clone()));
        }
        self.instrument(&browser);
        Ok((browser, page))
    }

    /// `page(n)` on `browser` (auto-creates up to `n`).
    pub async fn page_by_index(&self, browser: &Rc<Browser>, n: i64) -> Result<Page, RuntimeError> {
        if n < 1 {
            return Err(RuntimeError::new(format!(
                "page({n}): pages are numbered from 1"
            )));
        }
        let page = browser
            .page(n as usize)
            .await
            .map_err(|e| convert(e, "page"))?;
        self.instrument(browser);
        Ok(page)
    }

    /// `page("login")` on `browser` (auto-creates and names).
    pub async fn page_by_name(
        &self,
        browser: &Rc<Browser>,
        name: &str,
    ) -> Result<Page, RuntimeError> {
        let page = browser
            .page_named(name)
            .await
            .map_err(|e| convert(e, "page"))?;
        self.instrument(browser);
        Ok(page)
    }

    /// `browser.new_page(proxy:, name:, isolated:)`.
    pub async fn new_page(
        &self,
        browser: &Rc<Browser>,
        opts: NewPageOptions,
    ) -> Result<Page, RuntimeError> {
        let page = browser
            .new_page(opts)
            .await
            .map_err(|e| convert(e, "new_page"))?;
        self.instrument(browser);
        Ok(page)
    }

    /// If the default browser is live and has exactly one page, that page
    /// (synchronous peek for `page.index` / `emit page`).
    pub fn peek_sole_page(&self) -> Option<(Rc<Browser>, Page)> {
        let slot = self.default_slot("page").ok()?;
        let browser = slot.browser()?;
        let pages = browser.pages();
        if pages.len() == 1 {
            Some((browser, pages[0].clone()))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RuntimeOptions;

    #[test]
    fn several_named_browsers_without_default_is_an_error() {
        let rt = Runtime::new(RuntimeOptions::default());
        rt.declare_browser("work", Default::default());
        rt.declare_browser("home", Default::default());
        let msg = rt.default_slot("goto").unwrap_err().message;
        assert_eq!(
            msg,
            "several browsers are declared (\"work\", \"home\") — say which: work.goto(…)"
        );
    }

    #[test]
    fn one_named_browser_serves_bare_actions() {
        let rt = Runtime::new(RuntimeOptions::default());
        rt.declare_browser("work", Default::default());
        assert_eq!(rt.default_slot("goto").unwrap().alias, "work");
        let rt = Runtime::new(RuntimeOptions::default());
        assert_eq!(rt.default_slot("goto").unwrap().alias, DEFAULT_ALIAS);
    }
}
