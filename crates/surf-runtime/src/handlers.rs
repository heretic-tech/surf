//! Reactive handlers.
//!
//! | event              | source                                              | status |
//! |--------------------|-----------------------------------------------------|--------|
//! | `element_appears`  | isolated-world MutationObserver → `Runtime.bindingCalled` | live |
//! | `navigation`       | `Page.frameNavigated` (main frame)                  | live |
//! | `message`          | actor mailbox (`crate::actors::deliver`)             | live |
//! | `dialog`           | `Page.javascriptDialogOpening`                      | task 9 |
//! | `request`          | `Network.requestWillBeSent` (enables `Network`)     | task 9 |
//! | `response`         | `Network.responseReceived` (enables `Network`)      | task 9 |
//!
//! Registration is one mechanism for every event: `Host::declare` stores a
//! [`HandlerDecl`] and every page the runtime hands out gets one
//! **observer task** ([`spawn_observer`]) while any page-level handler is
//! declared; each firing runs the body on its own task with a forked VM
//! and `event` bound to the payload. After a `Page::rebind` (`fresh:
//! true`, `shift_proxy()`, supervisor restarts) the page's session is new,
//! so `Runtime::after_rebind` drops the old observer (its event channels
//! close with the detached session) and installs a fresh one.
//!
//! `on element_appears(sel):` (Decision 9). The observer task creates the page's isolated
//! world, registers a binding with `Runtime.addBinding{name: random,
//! executionContextId}`, installs a `MutationObserver` under random names
//! that resolves every handler's selector (through the world's own
//! resolver, so `text=` / `xpath=` work) and calls the binding once per
//! newly matching element, then listens for `Runtime.bindingCalled` —
//! which Chrome sends **without** `Runtime.enable`
//! (`crates/surf-browser/tests/pages.rs::binding_called_fires_without_runtime_enable`
//! and the `handler` e2e script both verify it; no polling fallback was
//! needed). On every main-frame `Page.frameNavigated` the world is gone, so
//! the observer is installed again in a fresh world. Each firing runs the
//! handler body on its own task with a forked VM, `event` bound to the
//! element, and bare actions bound to the page the element appeared on.
//!
//! `on navigation(pattern):` fires on every committed main-frame
//! navigation whose URL matches the glob (`*`) or `re:` pattern; `event`
//! is a map `{url, page}` and bare actions in the body act on that page.

use crate::host::Runtime;
use crate::objects::{ElementObject, PageObject};
use crate::pages::{TaskCtx, TASK_CTX};
use serde_json::json;
use std::rc::Rc;
use std::time::Duration;
use surf_browser::network::UrlPattern;
use surf_browser::util::random_ident;
use surf_browser::world::{by_value, context_lost};
use surf_browser::{Browser, BrowserError, Element, Page, Selector, World};
use surf_cdp::event;
use surf_vm::{Args, Closure, Value};

/// Handler kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandlerEvent {
    /// `on element_appears(selector):`
    ElementAppears,
    /// `on navigation(pattern):`
    Navigation,
    /// `on dialog:`
    Dialog,
    /// `on request(pattern):`
    Request,
    /// `on response(pattern):`
    Response,
    /// `on message:` (actors only)
    Message,
}

impl HandlerEvent {
    /// Parse the event name used after `on`.
    pub fn parse(name: &str) -> Option<HandlerEvent> {
        Some(match name {
            "element_appears" => HandlerEvent::ElementAppears,
            "navigation" => HandlerEvent::Navigation,
            "dialog" => HandlerEvent::Dialog,
            "request" => HandlerEvent::Request,
            "response" => HandlerEvent::Response,
            "message" => HandlerEvent::Message,
            _ => return None,
        })
    }

    /// The source-level name.
    pub fn name(self) -> &'static str {
        match self {
            HandlerEvent::ElementAppears => "element_appears",
            HandlerEvent::Navigation => "navigation",
            HandlerEvent::Dialog => "dialog",
            HandlerEvent::Request => "request",
            HandlerEvent::Response => "response",
            HandlerEvent::Message => "message",
        }
    }
}

/// A declared `on …:` block.
pub struct HandlerDecl {
    /// Event kind.
    pub event: HandlerEvent,
    /// Evaluated arguments (the selector for `element_appears`).
    pub args: Vec<Value>,
    /// Body closure taking `event`.
    pub body: Rc<Closure>,
}

impl HandlerDecl {
    /// The selector of an `element_appears` handler (or the pattern of a
    /// `navigation` handler).
    pub fn selector(&self) -> String {
        self.args.first().map(|v| v.to_string()).unwrap_or_default()
    }

    /// Whether a `navigation` handler's pattern matches `url` (no pattern
    /// matches everything; `re:` is a regex, otherwise a `*` glob).
    pub fn matches_url(&self, url: &str) -> bool {
        let Some(p) = self.args.first() else {
            return true;
        };
        let p = p.to_string();
        if p.is_empty() {
            return true;
        }
        if let Some(re) = p.strip_prefix("re:") {
            return regex::Regex::new(re).is_ok_and(|r| r.is_match(url));
        }
        UrlPattern(p).matches(url)
    }
}

/// Installed in the isolated world: `this` is the world's global object.
/// Arguments: store name, binding name, resolver name, `[[kind, text], …]`.
const OBSERVER_SOURCE: &str = r#"function(store, binding, resolver, selectors) {
  const list = [];
  const seen = selectors.map(() => new WeakSet());
  this[store] = list;
  const scan = () => {
    for (let h = 0; h < selectors.length; h++) {
      let found;
      try { found = this[resolver](selectors[h][0], selectors[h][1], true) || []; } catch (e) { found = []; }
      for (const el of found) {
        if (seen[h].has(el)) continue;
        seen[h].add(el);
        const i = list.push(el) - 1;
        try { this[binding](JSON.stringify({ i: i, h: h })); } catch (e) {}
      }
    }
  };
  const mo = new MutationObserver(() => scan());
  mo.observe(document, { childList: true, subtree: true, attributes: true, characterData: true });
  scan();
  return true;
}"#;

/// Attempts to (re)install the observer after a navigation before giving up.
const INSTALL_ATTEMPTS: usize = 4;

struct Installed {
    world: World,
    binding: String,
    store: String,
}

async fn install(page: &Page, handlers: &[Rc<HandlerDecl>]) -> Result<Installed, BrowserError> {
    let selectors: Vec<(String, String)> = handlers
        .iter()
        .map(|h| {
            let sel = Selector::parse(&h.selector());
            let (kind, text) = sel.parts();
            (kind.to_owned(), text.to_owned())
        })
        .collect();
    let mut last = None;
    for attempt in 0..INSTALL_ATTEMPTS {
        if attempt > 0 {
            page.invalidate_world();
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let world = match page.world().await {
            Ok(w) => w,
            Err(e) if context_lost(&e) => {
                last = Some(e);
                continue;
            }
            Err(e) => return Err(e),
        };
        let binding = format!("__surf_on_{}", random_ident(10));
        let store = format!("__surf_seen_{}", random_ident(10));
        let r = async {
            world.add_binding(&binding).await?;
            world
                .call(
                    OBSERVER_SOURCE,
                    vec![
                        json!(store),
                        json!(binding),
                        json!(world.resolver),
                        json!(selectors),
                    ],
                )
                .await?;
            Ok::<_, BrowserError>(())
        }
        .await;
        match r {
            Ok(()) => {
                return Ok(Installed {
                    world,
                    binding,
                    store,
                })
            }
            Err(e) if context_lost(&e) => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| BrowserError::Unsupported("observer install failed".into())))
}

/// Start the observer task for `page` (no-op without `element_appears` /
/// `navigation` handlers). Counts as an installed handler for the
/// lifetime rule until the page's session ends.
pub fn spawn_observer(rt: Rc<Runtime>, browser: Rc<Browser>, page: Page) {
    let handlers = rt.element_handlers();
    let nav_handlers = rt.navigation_handlers();
    if handlers.is_empty() && nav_handlers.is_empty() {
        return;
    }
    rt.lifetime().handler_installed();
    tokio::task::spawn_local(async move {
        observe(rt.clone(), browser, page, handlers, nav_handlers).await;
        rt.lifetime().handler_removed();
    });
}

async fn observe(
    rt: Rc<Runtime>,
    browser: Rc<Browser>,
    page: Page,
    handlers: Vec<Rc<HandlerDecl>>,
    nav_handlers: Vec<Rc<HandlerDecl>>,
) {
    let Ok(session) = page.session() else {
        return;
    };
    let frame_id = page.frame_id().unwrap_or_default();
    let mut bindings = session.events("Runtime.bindingCalled");
    let mut navigations = session.events("Page.frameNavigated");
    let mut installed = if handlers.is_empty() {
        None
    } else {
        match install(&page, &handlers).await {
            Ok(i) => Some(i),
            Err(e) => {
                if page.is_open() {
                    rt.warn(&format!(
                        "on element_appears: could not observe page {}: {e}",
                        page.label()
                    ));
                }
                return;
            }
        }
    };
    tracing::debug!(
        "observer on page {} ({} element_appears, {} navigation)",
        page.label(),
        handlers.len(),
        nav_handlers.len()
    );
    loop {
        tokio::select! {
            ev = event::next(&mut bindings) => {
                let Some(ev) = ev else { break };
                let Some(inst) = &installed else { continue };
                if ev.params["name"] != inst.binding {
                    continue;
                }
                let payload: serde_json::Value = ev.params["payload"]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or(serde_json::Value::Null);
                let (Some(i), Some(h)) = (payload["i"].as_u64(), payload["h"].as_u64()) else {
                    continue;
                };
                let Some(handler) = handlers.get(h as usize) else {
                    continue;
                };
                dispatch(
                    rt.clone(),
                    browser.clone(),
                    page.clone(),
                    inst.world.clone(),
                    inst.store.clone(),
                    handler.clone(),
                    i,
                );
            }
            ev = event::next(&mut navigations) => {
                let Some(ev) = ev else { break };
                if ev.params["frame"]["id"] != frame_id.as_str() {
                    continue;
                }
                let url = ev.params["frame"]["url"].as_str().unwrap_or("").to_owned();
                for h in &nav_handlers {
                    if h.matches_url(&url) {
                        dispatch_navigation(rt.clone(), browser.clone(), page.clone(), h.clone(), &url);
                    }
                }
                if handlers.is_empty() {
                    continue;
                }
                // The observer may have been installed after this navigation
                // committed (the task started while a `goto` was in flight);
                // an isolated world only survives within one document, so a
                // world that still answers already covers this document.
                if let Some(inst) = &installed {
                    if still_installed(inst).await {
                        continue;
                    }
                }
                page.invalidate_world();
                match install(&page, &handlers).await {
                    Ok(i) => installed = Some(i),
                    Err(e) => {
                        if page.is_open() {
                            rt.warn(&format!(
                                "on element_appears: lost page {} after navigation: {e}",
                                page.label()
                            ));
                        }
                        break;
                    }
                }
            }
        }
    }
}

/// Whether the installed observer's world (and so its document) is still
/// alive.
async fn still_installed(inst: &Installed) -> bool {
    inst.world
        .call(
            "function(s) { return Array.isArray(this[s]); }",
            vec![json!(inst.store)],
        )
        .await
        .map(|v| v == serde_json::Value::Bool(true))
        .unwrap_or(false)
}

/// Run one `navigation` handler invocation on its own task.
fn dispatch_navigation(
    rt: Rc<Runtime>,
    browser: Rc<Browser>,
    page: Page,
    handler: Rc<HandlerDecl>,
    url: &str,
) {
    rt.lifetime().task_started();
    let mut event: indexmap::IndexMap<Rc<str>, Value> = indexmap::IndexMap::new();
    event.insert(Rc::from("url"), Value::str(url));
    event.insert(
        Rc::from("page"),
        PageObject::bound(rt.clone(), browser.clone(), page.clone()),
    );
    let event = Value::map(event);
    tokio::task::spawn_local(async move {
        let ctx = TaskCtx::handler(browser, page);
        let mut vm = rt.vm();
        let r = TASK_CTX
            .scope(
                ctx,
                vm.call(handler.body.clone(), Args::positional(vec![event])),
            )
            .await;
        rt.task_result("on navigation", r);
        rt.lifetime().task_finished();
    });
}

/// Run one handler invocation on its own task.
fn dispatch(
    rt: Rc<Runtime>,
    browser: Rc<Browser>,
    page: Page,
    world: World,
    store: String,
    handler: Rc<HandlerDecl>,
    index: u64,
) {
    rt.lifetime().task_started();
    let rt2 = rt.clone();
    tokio::task::spawn_local(async move {
        let label = format!("{} (appeared)", handler.selector());
        let decl = format!("function(i) {{ return this[{store:?}][i]; }}");
        let element = match world.call_handle(&decl, by_value(vec![json!(index)])).await {
            Ok(r) => r
                .object_id
                .map(|id| Element::from_handle(page.clone(), world.clone(), id, label)),
            Err(e) => {
                tracing::debug!("element_appears: handle lookup failed: {e}");
                None
            }
        };
        let event = match element {
            Some(el) => ElementObject::value(&rt, &browser, &page, el),
            None => Value::Nil,
        };
        let ctx = TaskCtx::handler(browser.clone(), page.clone());
        let mut vm = rt.vm();
        let r = TASK_CTX
            .scope(
                ctx,
                vm.call(handler.body.clone(), Args::positional(vec![event])),
            )
            .await;
        rt.task_result(&format!("on {}", handler.event.name()), r);
        rt2.lifetime().task_finished();
    });
}
