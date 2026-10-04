//! The [`Page`] handle and its swappable backing.
//!
//! Decision 10: page identity is decoupled from the CDP target. A script's
//! `page` value stays valid across supervisor restarts, `shift_proxy()` and
//! (later) persona switches because the handle is `Rc<PageInner>` and only
//! the [`Backing`] inside is replaced.

use crate::error::BrowserError;
use crate::world::World;
use std::cell::RefCell;
use std::rc::Rc;
use surf_cdp::Session;

/// The CDP identity currently behind a page.
#[derive(Clone, Debug)]
pub struct Backing {
    /// Attached session for the target.
    pub session: Session,
    /// `Target.targetId`.
    pub target_id: String,
    /// Browser context the target lives in (`None` → default context).
    pub browser_context_id: Option<String>,
    /// Main frame id (for `Page.createIsolatedWorld`).
    pub frame_id: String,
}

/// What to carry over when a page is rebound.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Migration {
    /// Copy cookies via `Storage.getCookies` / `Storage.setCookies`.
    pub cookies: bool,
    /// Copy `localStorage` / `sessionStorage` via isolated-world eval.
    pub storage: bool,
    /// Navigate the new target to the old URL.
    pub url: bool,
}

impl Migration {
    /// Everything.
    pub const ALL: Migration = Migration {
        cookies: true,
        storage: true,
        url: true,
    };
}

/// Shared page state.
pub struct PageInner {
    backing: RefCell<Option<Backing>>,
    world: RefCell<Option<World>>,
    /// Creation index (1-based) — `page(2)`.
    pub index: usize,
    /// Optional user name — `page("login")`.
    pub name: RefCell<Option<String>>,
}

/// A stable handle to a tab. Cheap to clone; all clones share state.
#[derive(Clone)]
pub struct Page(Rc<PageInner>);

impl Page {
    /// Wrap a backing as page number `index`.
    pub fn new(index: usize, backing: Backing) -> Page {
        Page(Rc::new(PageInner {
            backing: RefCell::new(Some(backing)),
            world: RefCell::new(None),
            index,
            name: RefCell::new(None),
        }))
    }

    /// Creation index (1-based).
    pub fn index(&self) -> usize {
        self.0.index
    }

    /// Current backing (clone). `None` after `close()`.
    pub fn backing(&self) -> Option<Backing> {
        self.0.backing.borrow().clone()
    }

    /// Current session, or `Closed`.
    pub fn session(&self) -> Result<Session, BrowserError> {
        self.backing()
            .map(|b| b.session)
            .ok_or(BrowserError::Cdp(surf_cdp::CdpError::Closed))
    }

    /// The isolated world (created lazily on first use, re-created after
    /// navigation / rebind).
    pub async fn world(&self) -> Result<World, BrowserError> {
        if let Some(w) = self.0.world.borrow().clone() {
            return Ok(w);
        }
        Err(BrowserError::Unsupported(
            "isolated world not implemented yet (task 4)".into(),
        ))
    }

    /// Replace the backing, migrating state per `migration`. The old
    /// target is closed after the new one is ready. Implemented in task 4
    /// (used by tasks 8 and later).
    pub async fn rebind(
        &self,
        new_backing: Backing,
        migration: Migration,
    ) -> Result<(), BrowserError> {
        let _ = migration;
        *self.0.world.borrow_mut() = None;
        *self.0.backing.borrow_mut() = Some(new_backing);
        Ok(())
    }

    /// Close the tab (`Target.closeTarget`) and drop the backing.
    pub async fn close(&self) -> Result<(), BrowserError> {
        *self.0.world.borrow_mut() = None;
        *self.0.backing.borrow_mut() = None;
        Ok(())
    }
}
