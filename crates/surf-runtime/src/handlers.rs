//! Reactive handlers.
//!
//! | event              | source                                              | status |
//! |--------------------|-----------------------------------------------------|--------|
//! | `element_appears`  | isolated-world MutationObserver → `Runtime.bindingCalled` | this step |
//! | `navigation`       | `Page.frameNavigated` (main frame)                  | task 8 |
//! | `dialog`           | `Page.javascriptDialogOpening`                      | task 8 |
//! | `request`          | `Network.requestWillBeSent` (enables `Network`)     | task 9 |
//! | `response`         | `Network.responseReceived` (enables `Network`)      | task 9 |
//! | `message`          | actor mailbox                                        | task 8 |
//!
//! `on element_appears(sel):` (Decision 9). Every page the runtime hands
//! out gets one **observer task** ([`spawn_observer`]) as long as at least
//! one such handler is declared. The task creates the page's isolated
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

use crate::host::Runtime;
use crate::objects::ElementObject;
use crate::pages::{TaskCtx, TASK_CTX};
use serde_json::json;
use std::rc::Rc;
use std::time::Duration;
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
    /// The selector of an `element_appears` handler.
    pub fn selector(&self) -> String {
        self.args.first().map(|v| v.to_string()).unwrap_or_default()
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

/// Start the observer task for `page` (no-op without `element_appears`
/// handlers). Counts as an installed handler for the lifetime rule until
/// the page's session ends.
pub fn spawn_observer(rt: Rc<Runtime>, browser: Rc<Browser>, page: Page) {
    let handlers = rt.element_handlers();
    if handlers.is_empty() {
        return;
    }
    rt.lifetime().handler_installed();
    tokio::task::spawn_local(async move {
        observe(rt.clone(), browser, page, handlers).await;
        rt.lifetime().handler_removed();
    });
}

async fn observe(
    rt: Rc<Runtime>,
    browser: Rc<Browser>,
    page: Page,
    handlers: Vec<Rc<HandlerDecl>>,
) {
    let Ok(session) = page.session() else {
        return;
    };
    let frame_id = page.frame_id().unwrap_or_default();
    let mut bindings = session.events("Runtime.bindingCalled");
    let mut navigations = session.events("Page.frameNavigated");
    let mut installed = match install(&page, &handlers).await {
        Ok(i) => i,
        Err(e) => {
            if page.is_open() {
                rt.warn(&format!(
                    "on element_appears: could not observe page {}: {e}",
                    page.label()
                ));
            }
            return;
        }
    };
    tracing::debug!(
        "element_appears observer on page {} (binding {})",
        page.label(),
        installed.binding
    );
    loop {
        tokio::select! {
            ev = event::next(&mut bindings) => {
                let Some(ev) = ev else { break };
                if ev.params["name"] != installed.binding {
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
                    installed.world.clone(),
                    installed.store.clone(),
                    handler.clone(),
                    i,
                );
            }
            ev = event::next(&mut navigations) => {
                let Some(ev) = ev else { break };
                if ev.params["frame"]["id"] != frame_id.as_str() {
                    continue;
                }
                page.invalidate_world();
                match install(&page, &handlers).await {
                    Ok(i) => installed = i,
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
