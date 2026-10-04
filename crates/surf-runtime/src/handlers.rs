//! Reactive handlers.
//!
//! | event              | source                                                    |
//! |--------------------|-----------------------------------------------------------|
//! | `element_appears`  | isolated-world MutationObserver → `Runtime.bindingCalled` |
//! | `navigation`       | `Page.frameNavigated` (main frame)                        |
//! | `dialog`           | `Page.javascriptDialogOpening` (policy `Defer`)           |
//! | `request`          | `Network.requestWillBeSent` (enables `Network`)           |
//! | `response`         | `Network.responseReceived` (enables `Network`)            |
//! | `intercept`        | `Fetch.requestPaused` (enables `Fetch`, shared with proxy auth) |
//! | `message`          | actor mailbox (`crate::actors::deliver`)                   |
//!
//! Registration is one mechanism for every page-level event: `Host::declare`
//! stores a [`HandlerDecl`] and every page the runtime hands out gets one
//! **observer task** ([`spawn_observer`]) while any page-level handler is
//! declared; each firing runs the body on its own task with a forked VM
//! and `event` bound to the payload. After a `Page::rebind` (`fresh:
//! true`, `shift_proxy()`, supervisor restarts) the page's session is new,
//! so `Runtime::after_rebind` drops the old observer (its event channels
//! close with the detached session) and installs a fresh one.
//!
//! `on element_appears(sel):` (Decision 9). The observer task creates the
//! page's isolated world, registers a binding with
//! `Runtime.addBinding{name: random, executionContextId}`, installs a
//! `MutationObserver` under random names that resolves every handler's
//! selector (through the world's own resolver, so `text=` / `xpath=` work)
//! and calls the binding once per newly matching element, then listens for
//! `Runtime.bindingCalled` — which Chrome sends **without** `Runtime.enable`
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
//!
//! `on request(pattern):` / `on response(pattern):` hold `Network` on the
//! page session only while the observer lives
//! ([`surf_browser::network::NetworkHooks`]); `event`
//! is a map for requests and a [`ResponseObject`] (with `body()`, which
//! waits for `Network.loadingFinished` before `Network.getResponseBody`)
//! for responses. `intercept(pattern):` routes matching `Fetch.requestPaused`
//! events to the body as an [`InterceptObject`]; a body that decides
//! nothing continues the request untouched. `on dialog:` switches the
//! page's dialog policy to `Defer` and hands the body a [`DialogObject`];
//! an undecided dialog is accepted when the body returns.

use crate::host::Runtime;
use crate::objects::{DialogObject, ElementObject, InterceptObject, PageObject, ResponseObject};
use crate::pages::{TaskCtx, TASK_CTX};
use serde_json::json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;
use surf_browser::network::{
    InterceptedRequest, Interception, RequestInfo, ResponseInfo, UrlPattern,
};
use surf_browser::util::random_ident;
use surf_browser::world::{by_value, context_lost};
use surf_browser::{Browser, BrowserError, DialogPolicy, Element, Page, Selector, World};
use surf_cdp::{event, Event};
use surf_vm::{Args, Closure, RuntimeError, Value};
use tokio::sync::{broadcast, Notify};

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
    /// `intercept(pattern):` / `on intercept(pattern):`
    Intercept,
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
            "intercept" => HandlerEvent::Intercept,
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
            HandlerEvent::Intercept => "intercept",
            HandlerEvent::Message => "message",
        }
    }

    /// Whether handlers of this kind are installed per page (everything
    /// but `message`).
    pub fn is_page_level(self) -> bool {
        !matches!(self, HandlerEvent::Message)
    }

    /// Whether the first argument is a URL pattern.
    fn takes_url_pattern(self) -> bool {
        matches!(
            self,
            HandlerEvent::Navigation
                | HandlerEvent::Request
                | HandlerEvent::Response
                | HandlerEvent::Intercept
        )
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
    /// The URL pattern of a `navigation` / `request` / `response` /
    /// `intercept` handler (`None` matches everything).
    pattern: Option<UrlPattern>,
}

impl HandlerDecl {
    /// Build a declaration; a bad `re:` pattern is an error.
    pub fn new(
        event: HandlerEvent,
        args: Vec<Value>,
        body: Rc<Closure>,
    ) -> Result<HandlerDecl, RuntimeError> {
        let pattern = if event.takes_url_pattern() {
            match args.first() {
                Some(v) if !v.is_nil() && !v.to_string().is_empty() => Some(
                    UrlPattern::parse(&v.to_string())
                        .map_err(|e| crate::errors::convert(e, &format!("on {}", event.name())))?,
                ),
                _ => None,
            }
        } else {
            None
        };
        Ok(HandlerDecl {
            event,
            args,
            body,
            pattern,
        })
    }

    /// The selector of an `element_appears` handler (or the pattern of a
    /// URL handler, as written).
    pub fn selector(&self) -> String {
        self.args.first().map(|v| v.to_string()).unwrap_or_default()
    }

    /// Whether the handler's URL pattern matches `url` (no pattern matches
    /// everything; `re:` is a regex, otherwise a `*` glob).
    pub fn matches_url(&self, url: &str) -> bool {
        self.pattern.as_ref().is_none_or(|p| p.matches(url))
    }

    /// The URL pattern, `*` when none was given.
    pub fn url_pattern(&self) -> UrlPattern {
        self.pattern
            .clone()
            .unwrap_or_else(|| UrlPattern::glob("*"))
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

/// Bookkeeping for `response.body()`: which requests finished loading
/// (`Network.loadingFinished` / `loadingFailed`), shared by the observer and
/// the [`ResponseObject`]s it hands out.
#[derive(Default)]
pub struct Loading {
    done: RefCell<HashMap<String, Result<(), String>>>,
    notify: Notify,
}

impl Loading {
    fn finish(&self, request_id: &str, r: Result<(), String>) {
        self.done.borrow_mut().insert(request_id.to_owned(), r);
        self.notify.notify_waiters();
    }

    /// Wait until `request_id` finished loading (or failed), up to `timeout`.
    pub async fn wait(&self, request_id: &str, timeout: Duration) -> Result<(), RuntimeError> {
        let started = std::time::Instant::now();
        loop {
            if let Some(r) = self.done.borrow().get(request_id).cloned() {
                return r
                    .map_err(|e| RuntimeError::new(format!("body(): the request failed: {e}")));
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(RuntimeError::new(format!(
                    "body(): the response did not finish loading within {}",
                    surf_vm::format_duration(&timeout)
                )));
            }
            let _ = tokio::time::timeout(
                remaining.min(Duration::from_millis(50)),
                self.notify.notified(),
            )
            .await;
        }
    }
}

/// Install the handlers on `page` and start its observer task (no-op
/// without page-level handlers). The set-up — `Network.enable` for
/// request / response hooks, `Fetch.enable` patterns for `intercept`,
/// the `MutationObserver` for `element_appears`, the deferred dialog
/// policy — is awaited *here*, before the action that asked for the page
/// runs, so the first navigation cannot race ahead of the hooks. Counts
/// as an installed handler for the lifetime rule until the page's session
/// ends.
pub async fn spawn_observer(rt: Rc<Runtime>, browser: Rc<Browser>, page: Page) {
    let all = rt.page_handlers();
    if all.is_empty() {
        return;
    }
    let Some(observer) = prepare(&rt, &page, all).await else {
        return;
    };
    rt.lifetime().handler_installed();
    tokio::task::spawn_local(async move {
        observe(rt.clone(), browser, page, observer).await;
        rt.lifetime().handler_removed();
    });
}

/// `event::next` on an optional receiver; pends forever when `None`.
async fn maybe_next(rx: Option<&mut broadcast::Receiver<Event>>) -> Option<Event> {
    match rx {
        Some(rx) => event::next(rx).await,
        None => std::future::pending().await,
    }
}

async fn maybe_intercepted(i: Option<&mut Interception>) -> Option<InterceptedRequest> {
    match i {
        Some(i) => i.next().await,
        None => std::future::pending().await,
    }
}

fn of_kind(all: &[Rc<HandlerDecl>], kind: HandlerEvent) -> Vec<Rc<HandlerDecl>> {
    all.iter().filter(|h| h.event == kind).cloned().collect()
}

/// Everything an observer task needs, built by [`prepare`] before the
/// page is handed to the script.
struct Observer {
    frame_id: String,
    handlers: Vec<Rc<HandlerDecl>>,
    nav_handlers: Vec<Rc<HandlerDecl>>,
    request_handlers: Vec<Rc<HandlerDecl>>,
    response_handlers: Vec<Rc<HandlerDecl>>,
    intercept_handlers: Vec<Rc<HandlerDecl>>,
    dialog_handlers: Vec<Rc<HandlerDecl>>,
    bindings: broadcast::Receiver<Event>,
    navigations: broadcast::Receiver<Event>,
    installed: Option<Installed>,
    req_rx: Option<broadcast::Receiver<Event>>,
    resp_rx: Option<broadcast::Receiver<Event>>,
    fin_rx: Option<broadcast::Receiver<Event>>,
    fail_rx: Option<broadcast::Receiver<Event>>,
    /// Keeps `Network` enabled for as long as the task runs.
    hold: Option<Rc<surf_browser::network::NetworkHold>>,
    loading: Rc<Loading>,
    interception: Option<Interception>,
    dialogs: Option<broadcast::Receiver<Event>>,
}

/// Subscribe and enable what the declared handlers need on `page`.
/// `None` when the page is gone or the `element_appears` observer cannot
/// be installed (a warning is printed).
async fn prepare(rt: &Rc<Runtime>, page: &Page, all: Vec<Rc<HandlerDecl>>) -> Option<Observer> {
    let session = page.session().ok()?;
    let handlers = of_kind(&all, HandlerEvent::ElementAppears);
    let nav_handlers = of_kind(&all, HandlerEvent::Navigation);
    let request_handlers = of_kind(&all, HandlerEvent::Request);
    let response_handlers = of_kind(&all, HandlerEvent::Response);
    let intercept_handlers = of_kind(&all, HandlerEvent::Intercept);
    let dialog_handlers = of_kind(&all, HandlerEvent::Dialog);
    let frame_id = page.frame_id().unwrap_or_default();
    let bindings = session.events("Runtime.bindingCalled");
    let navigations = session.events("Page.frameNavigated");
    let installed = if handlers.is_empty() {
        None
    } else {
        match install(page, &handlers).await {
            Ok(i) => Some(i),
            Err(e) => {
                if page.is_open() {
                    rt.warn(&format!(
                        "on element_appears: could not observe page {}: {e}",
                        page.label()
                    ));
                }
                return None;
            }
        }
    };
    // Network hooks: `Network` is held by `hold` for as long as the task runs.
    let (req_rx, resp_rx, fin_rx, fail_rx, hold) =
        if request_handlers.is_empty() && response_handlers.is_empty() {
            (None, None, None, None, None)
        } else {
            match page.network_hooks().await {
                Ok(h) => {
                    let (hold, s) = h.into_parts();
                    (
                        Some(s.requests),
                        Some(s.responses),
                        Some(s.finished),
                        Some(s.failed),
                        Some(Rc::new(hold)),
                    )
                }
                Err(e) => {
                    if page.is_open() {
                        rt.warn(&format!(
                            "on request / on response: could not enable Network on page {}: {e}",
                            page.label()
                        ));
                    }
                    (None, None, None, None, None)
                }
            }
        };
    let interception = if intercept_handlers.is_empty() {
        None
    } else {
        let patterns = intercept_handlers.iter().map(|h| h.url_pattern()).collect();
        match page.intercept(patterns).await {
            Ok(i) => Some(i),
            Err(e) => {
                if page.is_open() {
                    rt.warn(&format!(
                        "intercept: could not enable Fetch on page {}: {e}",
                        page.label()
                    ));
                }
                None
            }
        }
    };
    let dialogs = if dialog_handlers.is_empty() {
        None
    } else {
        page.on_dialog(DialogPolicy::Defer);
        Some(session.events("Page.javascriptDialogOpening"))
    };
    tracing::debug!(
        "observer on page {} ({} element_appears, {} navigation, {} request, {} response, {} intercept, {} dialog)",
        page.label(),
        handlers.len(),
        nav_handlers.len(),
        request_handlers.len(),
        response_handlers.len(),
        intercept_handlers.len(),
        dialog_handlers.len()
    );
    Some(Observer {
        frame_id,
        handlers,
        nav_handlers,
        request_handlers,
        response_handlers,
        intercept_handlers,
        dialog_handlers,
        bindings,
        navigations,
        installed,
        req_rx,
        resp_rx,
        fin_rx,
        fail_rx,
        hold,
        loading: Rc::new(Loading::default()),
        interception,
        dialogs,
    })
}

async fn observe(rt: Rc<Runtime>, browser: Rc<Browser>, page: Page, observer: Observer) {
    let Observer {
        frame_id,
        handlers,
        nav_handlers,
        request_handlers,
        response_handlers,
        intercept_handlers,
        dialog_handlers,
        mut bindings,
        mut navigations,
        mut installed,
        mut req_rx,
        mut resp_rx,
        mut fin_rx,
        mut fail_rx,
        hold,
        loading,
        mut interception,
        mut dialogs,
    } = observer;
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
                        let mut event: indexmap::IndexMap<Rc<str>, Value> = indexmap::IndexMap::new();
                        event.insert(Rc::from("url"), Value::str(&url));
                        event.insert(
                            Rc::from("page"),
                            PageObject::bound(rt.clone(), browser.clone(), page.clone()),
                        );
                        dispatch_event(rt.clone(), browser.clone(), page.clone(), h.clone(), Value::map(event));
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
            ev = maybe_next(req_rx.as_mut()) => {
                let Some(ev) = ev else { break };
                let info = RequestInfo::from_event(&ev.params);
                for h in &request_handlers {
                    if h.matches_url(&info.url) {
                        let event = request_value(&rt, &browser, &page, &info);
                        dispatch_event(rt.clone(), browser.clone(), page.clone(), h.clone(), event);
                    }
                }
            }
            ev = maybe_next(resp_rx.as_mut()) => {
                let Some(ev) = ev else { break };
                let info = ResponseInfo::from_event(&ev.params);
                for h in &response_handlers {
                    if h.matches_url(&info.url) {
                        let Some(hold) = &hold else { continue };
                        let event = ResponseObject::value(
                            &rt,
                            &browser,
                            &page,
                            info.clone(),
                            hold.clone(),
                            loading.clone(),
                        );
                        dispatch_event(rt.clone(), browser.clone(), page.clone(), h.clone(), event);
                    }
                }
            }
            ev = maybe_next(fin_rx.as_mut()) => {
                let Some(ev) = ev else { break };
                if let Some(id) = ev.params["requestId"].as_str() {
                    loading.finish(id, Ok(()));
                }
            }
            ev = maybe_next(fail_rx.as_mut()) => {
                let Some(ev) = ev else { break };
                if let Some(id) = ev.params["requestId"].as_str() {
                    let text = ev.params["errorText"].as_str().unwrap_or("loading failed").to_owned();
                    loading.finish(id, Err(text));
                }
            }
            req = maybe_intercepted(interception.as_mut()) => {
                let Some(req) = req else { break };
                dispatch_intercept(rt.clone(), browser.clone(), page.clone(), intercept_handlers.clone(), req);
            }
            ev = maybe_next(dialogs.as_mut()) => {
                let Some(ev) = ev else { break };
                dispatch_dialog(rt.clone(), browser.clone(), page.clone(), dialog_handlers.clone(), &ev.params);
            }
        }
    }
    drop(interception);
    drop(hold);
}

/// `event` for `on request`: a map.
fn request_value(rt: &Rc<Runtime>, browser: &Rc<Browser>, page: &Page, r: &RequestInfo) -> Value {
    let mut m: indexmap::IndexMap<Rc<str>, Value> = indexmap::IndexMap::new();
    m.insert(Rc::from("url"), Value::str(&r.url));
    m.insert(Rc::from("method"), Value::str(&r.method));
    m.insert(Rc::from("headers"), headers_value(&r.headers));
    m.insert(Rc::from("resource_type"), Value::str(&r.resource_type));
    m.insert(
        Rc::from("post_data"),
        r.post_data.as_deref().map(Value::str).unwrap_or(Value::Nil),
    );
    m.insert(Rc::from("request_id"), Value::str(&r.request_id));
    m.insert(
        Rc::from("page"),
        PageObject::bound(rt.clone(), browser.clone(), page.clone()),
    );
    Value::map(m)
}

/// A header list as a map value.
pub fn headers_value(headers: &[(String, String)]) -> Value {
    let mut m: indexmap::IndexMap<Rc<str>, Value> = indexmap::IndexMap::new();
    for (k, v) in headers {
        m.insert(Rc::from(k.as_str()), Value::str(v));
    }
    Value::map(m)
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

/// Run one handler invocation with `event` on its own task, bound to `page`.
fn dispatch_event(
    rt: Rc<Runtime>,
    browser: Rc<Browser>,
    page: Page,
    handler: Rc<HandlerDecl>,
    event: Value,
) {
    rt.lifetime().task_started();
    tokio::task::spawn_local(async move {
        let ctx = TaskCtx::handler(browser, page);
        let mut vm = rt.vm();
        let r = TASK_CTX
            .scope(
                ctx,
                vm.call(handler.body.clone(), Args::positional(vec![event])),
            )
            .await;
        rt.task_result(&format!("on {}", handler.event.name()), r);
        rt.lifetime().task_finished();
    });
}

/// Run the matching `intercept` handlers in declaration order until one
/// decides; continue the request untouched when none does.
fn dispatch_intercept(
    rt: Rc<Runtime>,
    browser: Rc<Browser>,
    page: Page,
    handlers: Vec<Rc<HandlerDecl>>,
    req: InterceptedRequest,
) {
    rt.lifetime().task_started();
    let req = Rc::new(req);
    tokio::task::spawn_local(async move {
        let ctx = TaskCtx::handler(browser.clone(), page.clone());
        for h in handlers.iter().filter(|h| h.matches_url(&req.request.url)) {
            if req.is_decided() {
                break;
            }
            let event = InterceptObject::value(&rt, &browser, &page, req.clone());
            let mut vm = rt.vm();
            let r = TASK_CTX
                .scope(
                    ctx.clone(),
                    vm.call(h.body.clone(), Args::positional(vec![event])),
                )
                .await;
            rt.task_result("intercept", r);
        }
        if !req.is_decided() {
            if let Err(e) = req.continue_request(Default::default()).await {
                tracing::debug!("intercept: default continue failed: {e}");
            }
        }
        rt.lifetime().task_finished();
    });
}

/// Run the `on dialog` handlers; accept the dialog when none decided.
fn dispatch_dialog(
    rt: Rc<Runtime>,
    browser: Rc<Browser>,
    page: Page,
    handlers: Vec<Rc<HandlerDecl>>,
    params: &serde_json::Value,
) {
    rt.lifetime().task_started();
    let dialog = Rc::new(DialogObject::new(
        page.clone(),
        params["type"].as_str().unwrap_or("").to_owned(),
        params["message"].as_str().unwrap_or("").to_owned(),
        params["defaultPrompt"].as_str().map(str::to_owned),
        params["url"].as_str().unwrap_or("").to_owned(),
    ));
    tokio::task::spawn_local(async move {
        let ctx = TaskCtx::handler(browser.clone(), page.clone());
        for h in &handlers {
            if dialog.is_decided() {
                break;
            }
            let event = DialogObject::value(&rt, &browser, dialog.clone());
            let mut vm = rt.vm();
            let r = TASK_CTX
                .scope(
                    ctx.clone(),
                    vm.call(h.body.clone(), Args::positional(vec![event])),
                )
                .await;
            rt.task_result("on dialog", r);
        }
        if !dialog.is_decided() {
            if let Err(e) = dialog.accept(None).await {
                tracing::debug!("on dialog: default accept failed: {e}");
            }
        }
        rt.lifetime().task_finished();
    });
}

/// Run one `element_appears` handler invocation on its own task.
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
