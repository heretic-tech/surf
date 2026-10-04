//! The [`Page`] handle and its swappable backing.
//!
//! Decision 10: page identity is decoupled from the CDP target. A script's
//! `page` value stays valid across supervisor restarts, `shift_proxy()` and
//! (later) persona switches because the handle is `Rc<PageInner>` and only
//! the [`Backing`] inside is replaced.

//!
//! Only `Page.enable` (ref-counted [`DomainGuard`]) is sent when a page is
//! attached, plus `Page.setLifecycleEventsEnabled` so navigations can be
//! awaited. One task per page follows `Page.frameNavigated` (to drop the
//! cached isolated world) and `Page.javascriptDialogOpening` (dialog
//! policy). Scripts' JavaScript only ever runs in the page's isolated
//! [`World`]; a world that Chrome reports destroyed is re-created once
//! transparently.

use crate::cookies::Cookie;
use crate::error::BrowserError;
use crate::launch::Credentials;
use crate::network::{FetchHub, Interception, NetworkHooks, UrlPattern};
use crate::util::base64_decode;
use crate::world::{context_lost, World};
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use surf_cdp::protocol::{emulation, page, storage, target};
use surf_cdp::{event, DomainGuard, Event, Session};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// Slack added to the page timeout for the per-call CDP timeout, so a
/// Surf-level timeout fires first with a better message.
const CALL_SLACK: Duration = Duration::from_secs(5);

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

impl Backing {
    /// Build a backing for an attached session, reading the main frame id
    /// from `Page.getFrameTree`.
    pub async fn discover(
        session: Session,
        target_id: String,
        browser_context_id: Option<String>,
    ) -> Result<Backing, BrowserError> {
        let tree = session.send(page::GetFrameTree {}).await?;
        Ok(Backing {
            session,
            target_id,
            browser_context_id,
            frame_id: tree.frame_tree.frame.id,
        })
    }
}

/// When `goto` / `reload` / `back` consider a navigation finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WaitUntil {
    /// `Page.lifecycleEvent{name: "load"}` (default).
    #[default]
    Load,
    /// `DOMContentLoaded`.
    DomContentLoaded,
    /// `networkIdle` (no network activity for 500 ms).
    NetworkIdle,
    /// `Page.navigate` returned; do not wait.
    Commit,
}

impl WaitUntil {
    /// Parse the `wait_until:` keyword values.
    pub fn parse(s: &str) -> Option<WaitUntil> {
        match s.to_ascii_lowercase().replace(['_', '-'], "").as_str() {
            "load" => Some(WaitUntil::Load),
            "domcontentloaded" => Some(WaitUntil::DomContentLoaded),
            "networkidle" => Some(WaitUntil::NetworkIdle),
            "commit" => Some(WaitUntil::Commit),
            _ => None,
        }
    }

    fn lifecycle_name(self) -> Option<&'static str> {
        match self {
            WaitUntil::Load => Some("load"),
            WaitUntil::DomContentLoaded => Some("DOMContentLoaded"),
            WaitUntil::NetworkIdle => Some("networkIdle"),
            WaitUntil::Commit => None,
        }
    }
}

/// What to do when the page opens an `alert` / `confirm` / `prompt` /
/// `beforeunload` dialog.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DialogPolicy {
    /// Accept (default). A prompt returns its default text, as if the
    /// user pressed OK.
    #[default]
    Accept,
    /// Dismiss / cancel.
    Dismiss,
    /// Accept with this text (prompts).
    AcceptWith(String),
    /// Leave the dialog open: an `on dialog:` handler answers it through
    /// [`Page::answer_dialog`].
    Defer,
}

/// A dialog the page opened (recorded for `page.dialogs()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    /// `alert`, `confirm`, `prompt`, `beforeunload`.
    pub kind: String,
    /// The message.
    pub message: String,
    /// The default prompt text, if a prompt.
    pub default_prompt: Option<String>,
    /// Page URL at the time.
    pub url: String,
}

/// State shared with the per-page event task (`Send`).
#[derive(Debug, Default)]
struct Shared {
    dialog_policy: Mutex<DialogPolicy>,
    dialogs: Mutex<Vec<Dialog>>,
    /// Bumped on every main-frame navigation; the cached world is only
    /// valid for the epoch it was created in.
    nav_epoch: AtomicU64,
    /// URL of the last committed main-frame navigation.
    last_url: Mutex<String>,
}

/// Navigation subscriptions taken at the start of an action that may
/// navigate (`click`, `press`, …). Broadcast receivers buffer from the
/// moment they are created, so a `wait_for_navigation` that follows the
/// action still sees a navigation that committed before it was called.
struct Armed {
    /// `nav_epoch` when the action started.
    epoch: u64,
    lifecycle: broadcast::Receiver<Event>,
    navigated: broadcast::Receiver<Event>,
}

/// Everything that belongs to one backing and is torn down on rebind.
struct Attached {
    backing: Backing,
    _page_guard: DomainGuard,
    events: JoinHandle<()>,
    /// The one owner of `Fetch` on this session (proxy auth + intercept).
    fetch: Rc<FetchHub>,
    /// `Network` held while `blocked` is non-empty.
    block_guard: Option<DomainGuard>,
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.events.abort();
    }
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
    attached: RefCell<Option<Attached>>,
    /// `(nav_epoch at creation, world)`.
    world: RefCell<Option<(u64, World)>>,
    shared: Arc<Shared>,
    /// Creation index (1-based) — `page(2)`.
    pub index: usize,
    /// Optional user name — `page("login")`.
    pub name: RefCell<Option<String>>,
    /// Default timeout for auto-waits and navigation.
    pub timeout: Cell<Duration>,
    /// Proxy credentials to re-install on rebind (same context kind).
    credentials: RefCell<Option<Credentials>>,
    /// `block([...])` patterns (Chrome dialect), re-applied on rebind.
    blocked: RefCell<Vec<String>>,
    /// Subscriptions taken by the last [`mark_action`](Page::mark_action).
    armed: RefCell<Option<Armed>>,
}

/// A stable handle to a tab. Cheap to clone; all clones share state.
#[derive(Clone)]
pub struct Page(Rc<PageInner>);

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Page")
            .field("index", &self.0.index)
            .field("name", &*self.0.name.borrow())
            .field("target", &self.backing().map(|b| b.target_id))
            .finish()
    }
}

impl Page {
    /// Attach page number `index` to `backing`: `Page.enable` (the only
    /// domain enabled by default), `Page.setLifecycleEventsEnabled`, the
    /// event task, and proxy authentication when `credentials` are given.
    pub async fn attach(
        index: usize,
        backing: Backing,
        timeout: Duration,
        credentials: Option<Credentials>,
    ) -> Result<Page, BrowserError> {
        let page = Page(Rc::new(PageInner {
            attached: RefCell::new(None),
            world: RefCell::new(None),
            shared: Arc::new(Shared::default()),
            index,
            name: RefCell::new(None),
            timeout: Cell::new(timeout),
            credentials: RefCell::new(credentials),
            blocked: RefCell::new(Vec::new()),
            armed: RefCell::new(None),
        }));
        page.install(backing).await?;
        Ok(page)
    }

    /// Enable what a backing needs and make it current.
    async fn install(&self, backing: Backing) -> Result<(), BrowserError> {
        let session = backing
            .session
            .with_timeout(Some(self.0.timeout.get() + CALL_SLACK));
        let page_guard = session.enable_domain("Page").await?;
        session
            .send(page::SetLifecycleEventsEnabled { enabled: true })
            .await?;
        let credentials = self.0.credentials.borrow().clone();
        let fetch = Rc::new(FetchHub::install(session.clone(), credentials).await?);
        let blocked = self.0.blocked.borrow().clone();
        let block_guard = if blocked.is_empty() {
            None
        } else {
            let guard = session.enable_domain("Network").await?;
            session
                .call_raw(
                    "Network.setBlockedURLs",
                    serde_json::json!({ "urls": blocked }),
                )
                .await?;
            Some(guard)
        };
        let events = tokio::spawn(page_events(
            session.clone(),
            self.0.shared.clone(),
            backing.frame_id.clone(),
        ));
        *self.0.world.borrow_mut() = None;
        self.0.armed.borrow_mut().take();
        *self.0.attached.borrow_mut() = Some(Attached {
            backing,
            _page_guard: page_guard,
            events,
            fetch,
            block_guard,
        });
        Ok(())
    }

    /// Creation index (1-based).
    pub fn index(&self) -> usize {
        self.0.index
    }

    /// The user-given name (`page("login")`), if any.
    pub fn name(&self) -> Option<String> {
        self.0.name.borrow().clone()
    }

    /// Set the user-given name.
    pub fn set_name(&self, name: Option<String>) {
        *self.0.name.borrow_mut() = name;
    }

    /// `1` or `"login"` — how the page is listed in errors.
    pub fn label(&self) -> String {
        match self.name() {
            Some(n) => format!("{n:?}"),
            None => self.0.index.to_string(),
        }
    }

    /// Default timeout for auto-waits and navigation.
    pub fn timeout(&self) -> Duration {
        self.0.timeout.get()
    }

    /// Change the default timeout.
    pub fn set_timeout(&self, d: Duration) {
        self.0.timeout.set(d);
    }

    /// Proxy credentials the next [`rebind`](Self::rebind) installs
    /// (`Browser::rebind_page_to` sets them when the context changes).
    pub(crate) fn set_credentials(&self, credentials: Option<Credentials>) {
        *self.0.credentials.borrow_mut() = credentials;
    }

    /// Whether the page is still open.
    pub fn is_open(&self) -> bool {
        self.0.attached.borrow().is_some()
    }

    /// Current backing (clone). `None` after `close()`.
    pub fn backing(&self) -> Option<Backing> {
        self.0.attached.borrow().as_ref().map(|a| a.backing.clone())
    }

    /// Current session (with the page's call timeout), or `PageClosed`.
    pub fn session(&self) -> Result<Session, BrowserError> {
        self.backing()
            .map(|b| {
                b.session
                    .with_timeout(Some(self.0.timeout.get() + CALL_SLACK))
            })
            .ok_or(BrowserError::PageClosed {
                index: self.0.index,
            })
    }

    /// Main frame id.
    pub fn frame_id(&self) -> Result<String, BrowserError> {
        self.backing()
            .map(|b| b.frame_id)
            .ok_or(BrowserError::PageClosed {
                index: self.0.index,
            })
    }

    /// The isolated world (created lazily on first use, re-created after
    /// navigation / rebind).
    pub async fn world(&self) -> Result<World, BrowserError> {
        let epoch = self.0.shared.nav_epoch.load(Ordering::SeqCst);
        if let Some((e, w)) = self.0.world.borrow().as_ref() {
            if *e == epoch {
                return Ok(w.clone());
            }
        }
        let session = self.session()?;
        let frame_id = self.frame_id()?;
        let w = World::create(session, &frame_id).await?;
        *self.0.world.borrow_mut() = Some((epoch, w.clone()));
        Ok(w)
    }

    /// Forget the cached world (next use creates a new one).
    pub fn invalidate_world(&self) {
        self.0.world.borrow_mut().take();
    }

    /// Run `f` against the world; if Chrome reports the execution context
    /// gone (navigation in between), create a fresh world and retry once.
    pub async fn with_world<T, F, Fut>(&self, f: F) -> Result<T, BrowserError>
    where
        F: Fn(World) -> Fut,
        Fut: Future<Output = Result<T, BrowserError>>,
    {
        let w = self.world().await?;
        match f(w).await {
            Err(e) if context_lost(&e) => {
                tracing::debug!("isolated world gone ({e}); re-creating");
                self.invalidate_world();
                let w = self.world().await?;
                f(w).await
            }
            r => r,
        }
    }

    /// Evaluate a JavaScript expression in the isolated world; promises
    /// are awaited; the result is a JSON value.
    pub async fn eval(&self, expression: &str) -> Result<Value, BrowserError> {
        self.with_world(|w| async move { w.eval(expression).await })
            .await
    }

    /// Call a JavaScript function source (`function(a, b) { … }` or an
    /// arrow function) with JSON arguments in the isolated world.
    pub async fn eval_fn(
        &self,
        function_source: &str,
        args: Vec<Value>,
    ) -> Result<Value, BrowserError> {
        self.with_world(|w| {
            let args = args.clone();
            async move { w.call(function_source, args).await }
        })
        .await
    }

    // ───────────────────────── navigation ─────────────────────────

    /// `Page.navigate` then wait for the matching lifecycle event.
    pub async fn goto(&self, url: &str, wait_until: WaitUntil) -> Result<(), BrowserError> {
        let timeout = self.0.timeout.get();
        let started = Instant::now();
        let session = self.session()?;
        let frame_id = self.frame_id()?;
        let mut lifecycle = session.events("Page.lifecycleEvent");
        let nav = session
            .send(page::Navigate {
                url: url.to_owned(),
                ..Default::default()
            })
            .await?;
        if let Some(reason) = nav.error_text {
            return Err(BrowserError::Navigation {
                url: url.to_owned(),
                reason,
            });
        }
        self.invalidate_world();
        let (Some(loader), Some(name)) = (nav.loader_id, wait_until.lifecycle_name()) else {
            // Same-document navigation (fragment) or `commit`: nothing to wait for.
            return Ok(());
        };
        let wait = async {
            while let Some(ev) = event::next(&mut lifecycle).await {
                let p = &ev.params;
                if p["frameId"] == frame_id && p["loaderId"] == loader && p["name"] == name {
                    return Ok(());
                }
            }
            Err(BrowserError::PageClosed {
                index: self.0.index,
            })
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(r) => r,
            Err(_) => Err(BrowserError::Timeout {
                action: "goto".into(),
                selector: None,
                waited_ms: started.elapsed().as_millis() as u64,
                last_state: Some(format!("waiting for {url} to reach {name}")),
            }),
        }
    }

    /// Subscribe to the navigation events *now*, before an action that
    /// may navigate runs, so that a `wait_for_navigation` issued after the
    /// action still sees a navigation that committed in between. Every
    /// action that can trigger a navigation (`click`, `press`, …) calls
    /// this first; the subscriptions are replaced by the next action and
    /// consumed by [`wait_for_navigation`](Self::wait_for_navigation).
    pub(crate) fn mark_action(&self) {
        let Some(backing) = self.backing() else {
            return;
        };
        let armed = Armed {
            epoch: self.0.shared.nav_epoch.load(Ordering::SeqCst),
            lifecycle: backing.session.events("Page.lifecycleEvent"),
            navigated: backing.session.events("Page.frameNavigated"),
        };
        *self.0.armed.borrow_mut() = Some(armed);
    }

    /// Wait for the next main-frame navigation to commit and reach
    /// `wait_until` (after a click that navigates, `back`, `reload`, …).
    /// Call it *after* triggering the navigation. Actions that can
    /// navigate subscribe to the navigation events before they run
    /// ([`mark_action`](Self::mark_action)); those buffered receivers are
    /// used here, so a navigation that committed between the action and
    /// this call is still seen. Without a preceding action the
    /// subscription is taken now and only a later navigation counts.
    pub async fn wait_for_navigation(&self, wait_until: WaitUntil) -> Result<(), BrowserError> {
        let session = self.session()?;
        let frame_id = self.frame_id()?;
        let armed = self.0.armed.borrow_mut().take();
        let (lifecycle, navigated) = match armed {
            Some(Armed {
                epoch,
                lifecycle,
                navigated,
            }) => {
                let now = self.0.shared.nav_epoch.load(Ordering::SeqCst);
                if now > epoch && wait_until.lifecycle_name().is_none() {
                    // Committed already and `commit` is all that was asked.
                    return Ok(());
                }
                (lifecycle, navigated)
            }
            None => (
                session.events("Page.lifecycleEvent"),
                session.events("Page.frameNavigated"),
            ),
        };
        self.await_navigation(lifecycle, navigated, &frame_id, wait_until)
            .await
    }

    async fn await_navigation(
        &self,
        mut lifecycle: tokio::sync::broadcast::Receiver<surf_cdp::Event>,
        mut navigated: tokio::sync::broadcast::Receiver<surf_cdp::Event>,
        frame_id: &str,
        wait_until: WaitUntil,
    ) -> Result<(), BrowserError> {
        let timeout = self.0.timeout.get();
        let started = Instant::now();
        let wait = async {
            let loader = loop {
                let Some(ev) = event::next(&mut navigated).await else {
                    return Err(BrowserError::PageClosed {
                        index: self.0.index,
                    });
                };
                let frame = &ev.params["frame"];
                if frame["id"] != frame_id {
                    continue;
                }
                self.invalidate_world();
                if ev.params["type"] == "BackForwardCacheRestore" {
                    return Ok(()); // restored, no load event follows
                }
                break frame["loaderId"].as_str().unwrap_or("").to_owned();
            };
            let Some(name) = wait_until.lifecycle_name() else {
                return Ok(());
            };
            while let Some(ev) = event::next(&mut lifecycle).await {
                let p = &ev.params;
                if p["frameId"] == frame_id && p["loaderId"] == loader && p["name"] == name {
                    return Ok(());
                }
            }
            Err(BrowserError::PageClosed {
                index: self.0.index,
            })
        };
        match tokio::time::timeout(timeout, wait).await {
            Ok(r) => r,
            Err(_) => Err(BrowserError::Timeout {
                action: "wait_for_navigation".into(),
                selector: None,
                waited_ms: started.elapsed().as_millis() as u64,
                last_state: None,
            }),
        }
    }

    /// `Page.reload` and wait for `wait_until`.
    pub async fn reload(&self, wait_until: WaitUntil) -> Result<(), BrowserError> {
        let session = self.session()?;
        let frame_id = self.frame_id()?;
        let lifecycle = session.events("Page.lifecycleEvent");
        let navigated = session.events("Page.frameNavigated");
        session.send(page::Reload::default()).await?;
        self.await_navigation(lifecycle, navigated, &frame_id, wait_until)
            .await
    }

    /// History back; `Ok(false)` when there is no previous entry.
    pub async fn back(&self, wait_until: WaitUntil) -> Result<bool, BrowserError> {
        self.history_step(-1, wait_until).await
    }

    /// History forward; `Ok(false)` when there is no next entry.
    pub async fn forward(&self, wait_until: WaitUntil) -> Result<bool, BrowserError> {
        self.history_step(1, wait_until).await
    }

    async fn history_step(&self, delta: i64, wait_until: WaitUntil) -> Result<bool, BrowserError> {
        let session = self.session()?;
        let frame_id = self.frame_id()?;
        let h = session.send(page::GetNavigationHistory {}).await?;
        let idx = h.current_index + delta;
        let Some(entry) = usize::try_from(idx).ok().and_then(|i| h.entries.get(i)) else {
            return Ok(false);
        };
        let lifecycle = session.events("Page.lifecycleEvent");
        let navigated = session.events("Page.frameNavigated");
        session
            .send(page::NavigateToHistoryEntry { entry_id: entry.id })
            .await?;
        self.await_navigation(lifecycle, navigated, &frame_id, wait_until)
            .await?;
        Ok(true)
    }

    /// Current URL (`location.href` in the isolated world, so fragments
    /// and `pushState` changes are included).
    pub async fn url(&self) -> Result<String, BrowserError> {
        Ok(self
            .eval("location.href")
            .await?
            .as_str()
            .unwrap_or("")
            .to_owned())
    }

    /// URL of the last main-frame navigation the event task saw (no round
    /// trip; may lag [`url`](Self::url) by a moment and ignores
    /// `pushState`).
    pub fn last_navigated_url(&self) -> String {
        self.0.shared.last_url.lock().expect("last_url").clone()
    }

    /// `document.title`.
    pub async fn title(&self) -> Result<String, BrowserError> {
        Ok(self
            .eval("document.title")
            .await?
            .as_str()
            .unwrap_or("")
            .to_owned())
    }

    // ───────────────────────── dialogs ─────────────────────────

    /// Set what happens to JavaScript dialogs (default: accept).
    pub fn on_dialog(&self, policy: DialogPolicy) {
        *self.0.shared.dialog_policy.lock().expect("dialog policy") = policy;
    }

    /// Dialogs the page has opened so far (oldest first).
    pub fn dialogs(&self) -> Vec<Dialog> {
        self.0.shared.dialogs.lock().expect("dialogs").clone()
    }

    /// `Page.handleJavaScriptDialog` for a dialog left open by
    /// [`DialogPolicy::Defer`].
    pub async fn answer_dialog(
        &self,
        accept: bool,
        prompt_text: Option<String>,
    ) -> Result<(), BrowserError> {
        self.session()?
            .send(page::HandleJavaScriptDialog {
                accept,
                prompt_text,
            })
            .await?;
        Ok(())
    }

    // ───────────────────────── network ─────────────────────────

    /// Enable `Network` on this page's session for `on request` /
    /// `on response` hooks; the domain is released when the returned
    /// handle is dropped (unless `block` still needs it).
    pub async fn network_hooks(&self) -> Result<NetworkHooks, BrowserError> {
        NetworkHooks::install(self.session()?).await
    }

    /// Route requests matching `patterns` to an `intercept(pattern):`
    /// handler ([`Interception`]); `Fetch` is shared with proxy
    /// authentication. Dropping the handle stops intercepting.
    pub async fn intercept(&self, patterns: Vec<UrlPattern>) -> Result<Interception, BrowserError> {
        let hub = self
            .0
            .attached
            .borrow()
            .as_ref()
            .map(|a| a.fetch.clone())
            .ok_or(BrowserError::PageClosed {
                index: self.0.index,
            })?;
        hub.intercept(patterns).await
    }

    /// `block([...])`: `Network.setBlockedURLs` with Chrome's `*`
    /// patterns (unanchored). This needs `Network` enabled, which is held
    /// while the list is non-empty and released by `block([])`. The list
    /// survives a rebind.
    pub async fn set_blocked_urls(&self, patterns: Vec<String>) -> Result<(), BrowserError> {
        let session = self.session()?;
        *self.0.blocked.borrow_mut() = patterns.clone();
        if patterns.is_empty() {
            let guard = self
                .0
                .attached
                .borrow_mut()
                .as_mut()
                .and_then(|a| a.block_guard.take());
            if guard.is_some() {
                session
                    .call_raw("Network.setBlockedURLs", serde_json::json!({ "urls": [] }))
                    .await?;
            }
            return Ok(());
        }
        let has_guard = self
            .0
            .attached
            .borrow()
            .as_ref()
            .is_some_and(|a| a.block_guard.is_some());
        if !has_guard {
            let guard = session.enable_domain("Network").await?;
            if let Some(a) = self.0.attached.borrow_mut().as_mut() {
                a.block_guard = Some(guard);
            }
        }
        session
            .call_raw(
                "Network.setBlockedURLs",
                serde_json::json!({ "urls": patterns }),
            )
            .await?;
        Ok(())
    }

    /// The current `block([...])` list.
    pub fn blocked_urls(&self) -> Vec<String> {
        self.0.blocked.borrow().clone()
    }

    // ───────────────────────── storage ─────────────────────────

    /// `localStorage` of the current origin as a map.
    pub async fn local_storage(&self) -> Result<serde_json::Map<String, Value>, BrowserError> {
        let v = self
            .eval("(() => { const o = {}; for (let i = 0; i < localStorage.length; i++) { const k = localStorage.key(i); o[k] = localStorage.getItem(k); } return o; })()")
            .await?;
        Ok(v.as_object().cloned().unwrap_or_default())
    }

    /// `localStorage.setItem` for every entry.
    pub async fn set_local_storage(
        &self,
        entries: serde_json::Map<String, Value>,
    ) -> Result<(), BrowserError> {
        self.eval_fn(
            "function(o) { for (const [k, v] of Object.entries(o)) localStorage.setItem(k, v === null || v === undefined ? '' : (typeof v === 'string' ? v : JSON.stringify(v))); }",
            vec![Value::Object(entries)],
        )
        .await?;
        Ok(())
    }

    /// `localStorage.clear()`.
    pub async fn clear_local_storage(&self) -> Result<(), BrowserError> {
        self.eval("(localStorage.clear(), null)").await?;
        Ok(())
    }

    // ───────────────────────── capture ─────────────────────────

    /// PNG screenshot bytes; `full_page` captures beyond the viewport.
    pub async fn screenshot_png(&self, full_page: bool) -> Result<Vec<u8>, BrowserError> {
        let session = self.session()?;
        let (clip, beyond) = if full_page {
            let m = session.send(page::GetLayoutMetrics {}).await?;
            let size = m.css_content_size;
            (
                Some(page::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: size.width.max(1.0),
                    height: size.height.max(1.0),
                    scale: 1.0,
                }),
                Some(true),
            )
        } else {
            (None, None)
        };
        let r = session
            .send(page::CaptureScreenshot {
                format: Some("png".into()),
                clip,
                capture_beyond_viewport: beyond,
                from_surface: Some(true),
                ..Default::default()
            })
            .await?;
        base64_decode(&r.data).map_err(BrowserError::Unsupported)
    }

    /// Write a PNG screenshot to `path`.
    pub async fn screenshot(&self, path: &Path, full_page: bool) -> Result<(), BrowserError> {
        let png = self.screenshot_png(full_page).await?;
        tokio::fs::write(path, png).await?;
        Ok(())
    }

    /// PDF bytes (`Page.printToPDF`; headless only — Chrome refuses it in
    /// headed mode).
    pub async fn pdf_bytes(&self) -> Result<Vec<u8>, BrowserError> {
        let r = self
            .session()?
            .send(page::PrintToPdf {
                print_background: Some(true),
                ..Default::default()
            })
            .await?;
        base64_decode(&r.data).map_err(BrowserError::Unsupported)
    }

    /// Write a PDF of the page to `path`.
    pub async fn pdf(&self, path: &Path) -> Result<(), BrowserError> {
        let bytes = self.pdf_bytes().await?;
        tokio::fs::write(path, bytes).await?;
        Ok(())
    }

    /// `Emulation.setDeviceMetricsOverride{width, height}`.
    pub async fn set_viewport(&self, width: u32, height: u32) -> Result<(), BrowserError> {
        self.session()?
            .send(emulation::SetDeviceMetricsOverride {
                width: i64::from(width),
                height: i64::from(height),
                device_scale_factor: 1.0,
                mobile: false,
                ..Default::default()
            })
            .await?;
        Ok(())
    }

    // ───────────────────────── cookies ─────────────────────────

    fn root_and_context(&self) -> Result<(Session, Option<String>), BrowserError> {
        let b = self.backing().ok_or(BrowserError::PageClosed {
            index: self.0.index,
        })?;
        let root = b
            .session
            .connection()
            .root()
            .with_timeout(Some(self.0.timeout.get() + CALL_SLACK));
        Ok((root, b.browser_context_id))
    }

    /// All cookies of this page's browser context (`Storage.getCookies`).
    pub async fn cookies(&self) -> Result<Vec<Cookie>, BrowserError> {
        let (root, ctx) = self.root_and_context()?;
        let r = root
            .send(storage::GetCookies {
                browser_context_id: ctx,
            })
            .await?;
        Ok(r.cookies.into_iter().map(Cookie::from_cdp).collect())
    }

    /// Set cookies in this page's browser context (`Storage.setCookies`).
    pub async fn set_cookies(&self, cookies: &[Cookie]) -> Result<(), BrowserError> {
        let (root, ctx) = self.root_and_context()?;
        root.send(storage::SetCookies {
            cookies: cookies.iter().map(Cookie::to_param).collect(),
            browser_context_id: ctx,
        })
        .await?;
        Ok(())
    }

    /// Clear every cookie of this page's browser context.
    pub async fn clear_cookies(&self) -> Result<(), BrowserError> {
        let (root, ctx) = self.root_and_context()?;
        root.send(storage::ClearCookies {
            browser_context_id: ctx,
        })
        .await?;
        Ok(())
    }

    // ───────────────────────── rebind / close ─────────────────────────

    /// Replace the backing, migrating state per `migration`: cookies via
    /// `Storage.getCookies` / `setCookies`, `localStorage` /
    /// `sessionStorage` via isolated-world evals (only when `url` is also
    /// migrated — storage is per origin), then the URL. The old target is
    /// closed after the new one is ready. The handle, its index and name
    /// stay the same, so script values keep working (Decision 10).
    pub async fn rebind(
        &self,
        new_backing: Backing,
        migration: Migration,
    ) -> Result<(), BrowserError> {
        let old = self.backing().ok_or(BrowserError::PageClosed {
            index: self.0.index,
        })?;
        let url = if migration.url {
            Some(self.url().await?)
        } else {
            None
        };
        let cookies = if migration.cookies {
            Some(self.cookies().await?)
        } else {
            None
        };
        let url_is_page = url
            .as_deref()
            .is_some_and(|u| !u.is_empty() && u != "about:blank");
        let storage = if migration.storage && url_is_page {
            Some(self.eval(STORAGE_EXPORT).await?)
        } else {
            None
        };

        // Tear down the old backing (aborts its task, releases `Page`).
        let old_attached = self.0.attached.borrow_mut().take();
        drop(old_attached);
        self.install(new_backing).await?;

        if let Some(cookies) = cookies {
            if !cookies.is_empty() {
                self.set_cookies(&cookies).await?;
            }
        }
        if let (Some(url), true) = (url.as_deref(), url_is_page) {
            self.goto(url, WaitUntil::Load).await?;
            if let Some(storage) = storage {
                self.eval_fn(STORAGE_IMPORT, vec![storage]).await?;
            }
        }
        let root = old.session.connection().root();
        let _ = old.session.detach().await;
        if let Err(e) = root
            .send(target::CloseTarget {
                target_id: old.target_id.clone(),
            })
            .await
        {
            tracing::debug!("closing old target {} after rebind: {e}", old.target_id);
        }
        Ok(())
    }

    /// Drop the backing without talking to Chrome (the browser process is
    /// gone or going away).
    pub(crate) fn forget(&self) {
        self.0.world.borrow_mut().take();
        self.0.armed.borrow_mut().take();
        self.0.attached.borrow_mut().take();
    }

    /// Close the tab (`Target.closeTarget`) and drop the backing.
    pub async fn close(&self) -> Result<(), BrowserError> {
        let Some(attached) = self.0.attached.borrow_mut().take() else {
            return Ok(());
        };
        self.0.world.borrow_mut().take();
        self.0.armed.borrow_mut().take();
        let backing = attached.backing.clone();
        drop(attached);
        let root = backing.session.connection().root();
        let _ = backing.session.detach().await;
        if backing.session.connection().is_closed() {
            return Ok(());
        }
        root.send(target::CloseTarget {
            target_id: backing.target_id,
        })
        .await?;
        Ok(())
    }
}

/// Export `localStorage` + `sessionStorage` as `{local: {...}, session: {...}}`.
const STORAGE_EXPORT: &str = r#"(() => {
  const dump = (s) => { const o = {}; for (let i = 0; i < s.length; i++) { const k = s.key(i); o[k] = s.getItem(k); } return o; };
  return { local: dump(localStorage), session: dump(sessionStorage) };
})()"#;

/// Import what [`STORAGE_EXPORT`] produced.
const STORAGE_IMPORT: &str = r#"function(data) {
  for (const [k, v] of Object.entries(data.local || {})) localStorage.setItem(k, v);
  for (const [k, v] of Object.entries(data.session || {})) sessionStorage.setItem(k, v);
}"#;

/// Per-page event task: main-frame navigations bump the world epoch;
/// dialogs are answered per policy. Ends when the session's channels close.
async fn page_events(session: Session, shared: Arc<Shared>, frame_id: String) {
    let mut navigated = session.events("Page.frameNavigated");
    let mut dialogs = session.events("Page.javascriptDialogOpening");
    loop {
        tokio::select! {
            ev = event::next(&mut navigated) => {
                let Some(ev) = ev else { break };
                let frame = &ev.params["frame"];
                if frame["id"] == frame_id.as_str() {
                    shared.nav_epoch.fetch_add(1, Ordering::SeqCst);
                    if let Some(url) = frame["url"].as_str() {
                        *shared.last_url.lock().expect("last_url") = url.to_owned();
                    }
                }
            }
            ev = event::next(&mut dialogs) => {
                let Some(ev) = ev else { break };
                let p = &ev.params;
                let dialog = Dialog {
                    kind: p["type"].as_str().unwrap_or("").to_owned(),
                    message: p["message"].as_str().unwrap_or("").to_owned(),
                    default_prompt: p["defaultPrompt"].as_str().map(str::to_owned),
                    url: p["url"].as_str().unwrap_or("").to_owned(),
                };
                let policy = shared.dialog_policy.lock().expect("dialog policy").clone();
                tracing::debug!("{} dialog {:?}: {policy:?}", dialog.kind, dialog.message);
                let (accept, prompt_text) = match policy {
                    DialogPolicy::Defer => {
                        shared.dialogs.lock().expect("dialogs").push(dialog);
                        continue;
                    }
                    DialogPolicy::Accept if dialog.kind == "prompt" => {
                        (true, Some(dialog.default_prompt.clone().unwrap_or_default()))
                    }
                    DialogPolicy::Accept => (true, None),
                    DialogPolicy::Dismiss => (false, None),
                    DialogPolicy::AcceptWith(t) => (true, Some(t)),
                };
                shared.dialogs.lock().expect("dialogs").push(dialog);
                if let Err(e) = session
                    .send(page::HandleJavaScriptDialog { accept, prompt_text })
                    .await
                {
                    tracing::debug!("Page.handleJavaScriptDialog: {e}");
                }
            }
        }
    }
}
