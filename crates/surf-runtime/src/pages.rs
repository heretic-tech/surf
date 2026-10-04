//! Implicit page resolution (`docs/language.md` § 5).
//!
//! The page registry itself lives in [`surf_browser::Browser`] (`page(n)`,
//! `page("name")`). This module adds the runtime rules on top: which
//! *browser* a bare action means, and the per-task **private page** —
//! inside a spawned task / actor / `parallel for` item the first bare
//! action creates a page private to that task (named `task#id`, in its
//! own browser context when the task is `fresh`), closed when the task
//! ends; a handler body is bound to the page its event fired on. Private
//! pages never count for the main body's "sole page" rule, so `spawn
//! worker()` followed by `goto(…)` in the main body still works; explicit
//! `page(n)` always reads the shared registry.
//!
//! [`Runtime::rebind`] is the one path every rebind takes (`fresh: true`
//! retries, `shift_proxy()`, supervisor restarts): it re-installs every
//! handler observer on the page's new session afterwards.

use crate::browsers::{several_browsers_message, BrowserSlot, DEFAULT_ALIAS};
use crate::errors::{ambiguous_message, convert};
use crate::host::Runtime;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use surf_browser::{Browser, Migration, NewPageOptions, Page, RebindTarget};
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
    /// Name the private page is registered under (`fetch#3`).
    pub label: String,
    /// Create the private page in its own browser context (`fresh: true`).
    pub isolated: Cell<bool>,
}

impl TaskCtx {
    /// A handler task bound to `page`.
    pub fn handler(browser: Rc<Browser>, page: Page) -> Rc<TaskCtx> {
        Rc::new(TaskCtx {
            kind: TaskKind::Handler,
            page: RefCell::new(Some((browser, page))),
            label: "handler".into(),
            isolated: Cell::new(false),
        })
    }

    /// A spawned task with no page yet; its first bare action creates one
    /// named `label` (isolated in its own context when `isolated`).
    pub fn spawned(label: &str, isolated: bool) -> Rc<TaskCtx> {
        Rc::new(TaskCtx {
            kind: TaskKind::Spawned,
            page: RefCell::new(None),
            label: label.to_owned(),
            isolated: Cell::new(isolated),
        })
    }

    /// A spawned task that inherits an existing page (supervisor restart).
    pub fn spawned_with(label: &str, browser: Rc<Browser>, page: Page) -> Rc<TaskCtx> {
        Rc::new(TaskCtx {
            kind: TaskKind::Spawned,
            page: RefCell::new(Some((browser, page))),
            label: label.to_owned(),
            isolated: Cell::new(false),
        })
    }

    /// The bound page, if open.
    pub fn open_page(&self) -> Option<(Rc<Browser>, Page)> {
        self.page.borrow().clone().filter(|(_, p)| p.is_open())
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
    /// inside a handler / spawned task (created on first use), otherwise
    /// the sole shared page of the default browser (created when none
    /// exists; an error listing the pages when several are open).
    pub async fn sole_page(&self, action: &str) -> Result<(Rc<Browser>, Page), RuntimeError> {
        let ctx = current_ctx();
        if let Some((b, p)) = ctx.as_ref().and_then(|c| c.open_page()) {
            return Ok((b, p));
        }
        let browser = self.default_browser(action).await?;
        let page = match &ctx {
            Some(ctx) => {
                let page = browser
                    .new_page(NewPageOptions {
                        name: Some(ctx.label.clone()),
                        isolated: ctx.isolated.get(),
                        proxy: None,
                    })
                    .await
                    .map_err(|e| convert(e, action))?;
                self.private_pages
                    .borrow_mut()
                    .insert((self.alias_of(&browser), page.index()));
                *ctx.page.borrow_mut() = Some((browser.clone(), page.clone()));
                page
            }
            None => self.shared_sole_page(&browser, action).await?,
        };
        self.instrument(&browser).await;
        Ok((browser, page))
    }

    /// The sole page of `browser` ignoring tasks' private pages: created
    /// when none exists, an error naming them when several are open.
    pub async fn shared_sole_page(
        &self,
        browser: &Rc<Browser>,
        action: &str,
    ) -> Result<Page, RuntimeError> {
        let alias = self.alias_of(browser);
        let shared: Vec<Page> = browser
            .pages()
            .into_iter()
            .filter(|p| {
                !self
                    .private_pages
                    .borrow()
                    .contains(&(alias.clone(), p.index()))
            })
            .collect();
        match shared.len() {
            0 => browser
                .new_page(NewPageOptions::default())
                .await
                .map_err(|e| convert(e, action)),
            1 => Ok(shared[0].clone()),
            _ => Err(RuntimeError::new(ambiguous_message(
                &shared.iter().map(Page::label).collect::<Vec<_>>(),
                action,
            ))),
        }
    }

    /// Whether `page` is a task's private page.
    pub fn is_private(&self, browser: &Rc<Browser>, page: &Page) -> bool {
        self.private_pages
            .borrow()
            .contains(&(self.alias_of(browser), page.index()))
    }

    /// Close a task's private page when the task ends (best effort; skipped
    /// when the program is exiting — shutdown closes everything).
    pub async fn release_private_page(&self, ctx: &TaskCtx) {
        let Some((browser, page)) = ctx.page.borrow_mut().take() else {
            return;
        };
        self.private_pages
            .borrow_mut()
            .remove(&(self.alias_of(&browser), page.index()));
        if self.lifetime().exit_code().is_some() || !page.is_open() {
            return;
        }
        if let Err(e) = browser.close_page(&page).await {
            tracing::debug!("closing private page {}: {e}", page.label());
        }
    }

    /// Move `page` onto a new target per `target`, then re-install every
    /// handler observer on its new session. The page the current task's
    /// bare actions use is unchanged (same handle).
    pub async fn rebind(
        &self,
        browser: &Rc<Browser>,
        page: &Page,
        target: RebindTarget,
        migration: Migration,
        action: &str,
    ) -> Result<(), RuntimeError> {
        browser
            .rebind_page_to(page, target, migration)
            .await
            .map_err(|e| convert(e, action))?;
        self.after_rebind(browser, page).await;
        Ok(())
    }

    /// `fresh: true` between attempts: move the current page (if any) to
    /// a new, empty browser context — cookies and storage gone, the proxy
    /// (if the page has one) kept.
    pub async fn fresh_page(&self, action: &str) -> Result<(), RuntimeError> {
        let Some((browser, page)) = self.current_page() else {
            return Ok(());
        };
        self.rebind(
            &browser,
            &page,
            RebindTarget::FreshContext,
            Migration::default(),
            action,
        )
        .await
    }

    /// The page the current code's bare actions resolve to, if it already
    /// exists: the task's private page, or the shared sole page of the
    /// default browser (`None` when nothing is open yet).
    pub fn current_page(&self) -> Option<(Rc<Browser>, Page)> {
        if let Some(ctx) = current_ctx() {
            return ctx.open_page();
        }
        let slot = self.default_slot("page").ok()?;
        let browser = slot.browser()?;
        let alias = self.alias_of(&browser);
        let shared: Vec<Page> = browser
            .pages()
            .into_iter()
            .filter(|p| {
                !self
                    .private_pages
                    .borrow()
                    .contains(&(alias.clone(), p.index()))
            })
            .collect();
        (shared.len() == 1).then(|| (browser, shared[0].clone()))
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
        self.instrument(browser).await;
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
        self.instrument(browser).await;
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
        self.instrument(browser).await;
        Ok(page)
    }

    /// If the current code's page already exists, that page (synchronous
    /// peek for `page.index` / `emit page`).
    pub fn peek_sole_page(&self) -> Option<(Rc<Browser>, Page)> {
        self.current_page()
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
