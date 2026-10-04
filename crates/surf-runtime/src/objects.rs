//! `NativeObject` wrappers exposed to scripts.
//!
//! - [`PageObject`]: `page` (lazy — binds to the sole page of the default
//!   browser on its first method call), `page(2)`, `page("login")`,
//!   `work.page(1)`.
//! - [`BrowserObject`]: `browser`, `work`; `page(n)`, `new_page(…)`,
//!   `pages()`, `close()`, and every page method forwarded to the sole page.
//! - [`ElementObject`]: an element handle from `all(sel)`, `first(sel)`,
//!   `wait(sel)` or the `event` of `on element_appears`.
//! - [`BareAction`]: a bare action referenced as a value (`f = click`).
//! - [`ResponseObject`]: the `event` of `on response` (`body()`).
//! - [`InterceptObject`]: the `event` of `intercept(pattern):`
//!   (`continue()`, `fulfil(…)`, `fail(…)`).
//! - [`DialogObject`]: the `event` of `on dialog` (`accept()`, `dismiss()`).

use crate::browsers::BrowserSlot;
use crate::errors::convert;
use crate::handlers::{headers_value, Loading};
use crate::host::Runtime;
use crate::methods::{did_you_mean, element_method, page_method, PAGE_METHODS};
use futures::future::LocalBoxFuture;
use std::any::Any;
use std::cell::Cell;
use std::rc::Rc;
use surf_browser::network::{ContinueOverrides, InterceptedRequest, NetworkHold, ResponseInfo};
use surf_browser::{Browser, Element, NewPageOptions, Page};
use surf_vm::{Args, NativeObject, RuntimeError, Value, Vm};

/// What a [`PageObject`] refers to.
#[derive(Clone)]
pub enum PageRef {
    /// The sole page of the default browser, resolved on first use.
    Sole,
    /// A concrete page.
    Bound(Rc<Browser>, Page),
}

/// `page`, `page(2)`, `work.page(1)`.
pub struct PageObject {
    rt: Rc<Runtime>,
    page: PageRef,
}

impl PageObject {
    /// The lazy `page` global.
    pub fn sole(rt: Rc<Runtime>) -> Value {
        Value::native(PageObject {
            rt,
            page: PageRef::Sole,
        })
    }

    /// A bound page.
    pub fn bound(rt: Rc<Runtime>, browser: Rc<Browser>, page: Page) -> Value {
        Value::native(PageObject {
            rt,
            page: PageRef::Bound(browser, page),
        })
    }

    async fn resolve(&self, action: &str) -> Result<(Rc<Browser>, Page), RuntimeError> {
        match &self.page {
            PageRef::Bound(b, p) => Ok((b.clone(), p.clone())),
            PageRef::Sole => self.rt.sole_page(action).await,
        }
    }

    fn peek(&self) -> Option<(Rc<Browser>, Page)> {
        match &self.page {
            PageRef::Bound(b, p) => Some((b.clone(), p.clone())),
            PageRef::Sole => self.rt.peek_sole_page(),
        }
    }
}

impl NativeObject for PageObject {
    fn type_name(&self) -> &str {
        "page"
    }

    fn call_method<'a>(
        &'a self,
        _vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            if name == "__call__" {
                // `page(2)` / `page("login")` on the lazy global.
                return page_call(&self.rt, None, args).await;
            }
            let (browser, page) = self.resolve(&name).await?;
            page_method(&self.rt, &browser, &page, &name, args).await
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        let (_, page) = self.peek()?;
        match name {
            "index" => Some(Value::Int(page.index() as i64)),
            "name" => Some(page.name().map(Value::str).unwrap_or(Value::Nil)),
            "open" => Some(Value::Bool(page.is_open())),
            _ => None,
        }
    }

    fn to_json(&self) -> Option<serde_json::Value> {
        let (_, page) = self.peek()?;
        Some(serde_json::json!({
            "index": page.index(),
            "name": page.name(),
            "url": page.last_navigated_url(),
        }))
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

/// `page(n)` / `page("name")` on `slot` (the default browser when `None`).
pub async fn page_call(
    rt: &Rc<Runtime>,
    slot: Option<Rc<BrowserSlot>>,
    args: Args,
) -> Result<Value, RuntimeError> {
    args.check_kwargs("page", &[])?;
    let slot = match slot {
        Some(s) => s,
        None => rt.default_slot("page")?,
    };
    let browser = slot.get(rt).await?;
    let page = match args.get(0) {
        None => browser
            .sole_page()
            .await
            .map_err(|e| crate::errors::convert(e, "page"))?,
        Some(Value::Int(n)) => rt.page_by_index(&browser, *n).await?,
        Some(Value::Str(s)) => rt.page_by_name(&browser, s).await?,
        Some(other) => {
            return Err(RuntimeError::new(format!(
                "page(): expected a page number or name, got {}",
                other.type_name()
            )))
        }
    };
    Ok(PageObject::bound(rt.clone(), browser, page))
}

/// `browser` / a named browser (`work`).
pub struct BrowserObject {
    rt: Rc<Runtime>,
    slot: Rc<BrowserSlot>,
}

impl BrowserObject {
    /// Wrap a slot.
    pub fn value(rt: Rc<Runtime>, slot: Rc<BrowserSlot>) -> Value {
        Value::native(BrowserObject { rt, slot })
    }
}

impl NativeObject for BrowserObject {
    fn type_name(&self) -> &str {
        "browser"
    }

    fn call_method<'a>(
        &'a self,
        _vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            match name.as_str() {
                "page" => page_call(&self.rt, Some(self.slot.clone()), args).await,
                "new_page" => {
                    args.check_kwargs("new_page", &["proxy", "name", "isolated"])?;
                    let browser = self.slot.get(&self.rt).await?;
                    let opts = NewPageOptions {
                        proxy: args.kw("proxy").and_then(|v| v.as_str().map(str::to_owned)),
                        name: args.kw("name").and_then(|v| v.as_str().map(str::to_owned)),
                        isolated: args.kw("isolated").is_some_and(Value::is_truthy),
                    };
                    let page = self.rt.new_page(&browser, opts).await?;
                    Ok(PageObject::bound(self.rt.clone(), browser, page))
                }
                "pages" => {
                    args.check_kwargs("pages", &[])?;
                    let Some(browser) = self.slot.browser() else {
                        return Ok(Value::list(vec![]));
                    };
                    Ok(Value::list(
                        browser
                            .pages()
                            .into_iter()
                            .map(|p| PageObject::bound(self.rt.clone(), browser.clone(), p))
                            .collect(),
                    ))
                }
                "close" => {
                    args.check_kwargs("close", &[])?;
                    self.slot.close().await;
                    Ok(Value::Nil)
                }
                "__call__" => Err(RuntimeError::new(
                    "a browser is not callable — use browser.page(n) or browser.goto(…)",
                )),
                action if PAGE_METHODS.contains(&action) => {
                    let browser = self.slot.get(&self.rt).await?;
                    let page = browser
                        .sole_page()
                        .await
                        .map_err(|e| crate::errors::convert(e, action))?;
                    self.rt.instrument(&browser).await;
                    page_method(&self.rt, &browser, &page, action, args).await
                }
                other => Err(RuntimeError::new(format!(
                    "browser has no method `{other}`{}",
                    did_you_mean(
                        other,
                        &["page", "new_page", "pages", "close", "goto", "click"]
                    )
                ))),
            }
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        match name {
            "name" => Some(Value::str(&self.slot.alias)),
            "launched" => Some(Value::Bool(self.slot.is_live())),
            _ => None,
        }
    }

    fn to_json(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "browser": self.slot.alias,
            "launched": self.slot.is_live(),
        }))
    }
}

/// An element handle.
pub struct ElementObject {
    rt: Rc<Runtime>,
    browser: Rc<Browser>,
    page: Page,
    element: Element,
}

impl ElementObject {
    /// Wrap an element.
    pub fn value(rt: &Runtime, browser: &Rc<Browser>, page: &Page, element: Element) -> Value {
        Value::native(ElementObject {
            rt: rt.rc(),
            browser: browser.clone(),
            page: page.clone(),
            element,
        })
    }
}

impl NativeObject for ElementObject {
    fn type_name(&self) -> &str {
        "element"
    }

    fn call_method<'a>(
        &'a self,
        _vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            element_method(
                &self.rt,
                &self.browser,
                &self.page,
                &self.element,
                &name,
                args,
            )
            .await
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        match name {
            "selector" => Some(Value::str(self.element.label())),
            "page" => Some(PageObject::bound(
                self.rt.clone(),
                self.browser.clone(),
                self.page.clone(),
            )),
            _ => None,
        }
    }

    fn to_json(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "selector": self.element.label() }))
    }
}

/// A bare action used as a value (`f = click`; `xs.each(click)`).
pub struct BareAction {
    rt: Rc<Runtime>,
    name: &'static str,
}

impl BareAction {
    /// Wrap the action called `name`.
    pub fn value(rt: Rc<Runtime>, name: &'static str) -> Value {
        Value::native(BareAction { rt, name })
    }
}

impl NativeObject for BareAction {
    fn type_name(&self) -> &str {
        "fn"
    }

    fn call_method<'a>(
        &'a self,
        vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            if name != "__call__" {
                return Err(RuntimeError::new(format!(
                    "fn `{}` has no method `{name}`",
                    self.name
                )));
            }
            crate::builtins::bare_action(&self.rt, vm, self.name, args).await
        })
    }
}

// ───────────────────────── network events ─────────────────────────

fn headers_from_value(v: &Value, f: &str) -> Result<Vec<(String, String)>, RuntimeError> {
    match v {
        Value::Nil => Ok(vec![]),
        Value::Map(m) => Ok(m
            .borrow()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()),
        other => Err(RuntimeError::new(format!(
            "{f}: `headers:` must be a map, got {}",
            other.type_name()
        ))),
    }
}

/// The `event` of `on response(pattern):`.
pub struct ResponseObject {
    rt: Rc<Runtime>,
    browser: Rc<Browser>,
    page: Page,
    info: ResponseInfo,
    hold: Rc<NetworkHold>,
    loading: Rc<Loading>,
}

impl ResponseObject {
    /// Wrap a response.
    pub fn value(
        rt: &Rc<Runtime>,
        browser: &Rc<Browser>,
        page: &Page,
        info: ResponseInfo,
        hold: Rc<NetworkHold>,
        loading: Rc<Loading>,
    ) -> Value {
        Value::native(ResponseObject {
            rt: rt.clone(),
            browser: browser.clone(),
            page: page.clone(),
            info,
            hold,
            loading,
        })
    }

    fn json(&self) -> serde_json::Value {
        let headers: serde_json::Map<String, serde_json::Value> = self
            .info
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        serde_json::json!({
            "url": self.info.url,
            "status": self.info.status,
            "status_text": self.info.status_text,
            "headers": headers,
            "mime_type": self.info.mime_type,
            "resource_type": self.info.resource_type,
            "from_cache": self.info.from_cache,
            "request_id": self.info.request_id,
        })
    }
}

impl NativeObject for ResponseObject {
    fn type_name(&self) -> &str {
        "response"
    }

    fn call_method<'a>(
        &'a self,
        _vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            match name.as_str() {
                "body" | "text" => {
                    args.check_kwargs(&name, &["timeout"])?;
                    let timeout = match args.kw("timeout") {
                        Some(Value::Duration(d)) => *d,
                        _ => self.page.timeout(),
                    };
                    self.loading.wait(&self.info.request_id, timeout).await?;
                    let body = self
                        .hold
                        .response_body(&self.info.request_id)
                        .await
                        .map_err(|e| convert(e, "body"))?;
                    Ok(Value::str(body.text()))
                }
                "json" => {
                    args.check_kwargs(&name, &["timeout"])?;
                    let timeout = match args.kw("timeout") {
                        Some(Value::Duration(d)) => *d,
                        _ => self.page.timeout(),
                    };
                    self.loading.wait(&self.info.request_id, timeout).await?;
                    let body = self
                        .hold
                        .response_body(&self.info.request_id)
                        .await
                        .map_err(|e| convert(e, "json"))?;
                    let v: serde_json::Value = serde_json::from_slice(&body.bytes)
                        .map_err(|e| RuntimeError::new(format!("json(): {e}")))?;
                    Ok(Value::from_json(v))
                }
                "header" => {
                    args.check_kwargs(&name, &[])?;
                    let key = args.require(0, &name)?.to_string().to_ascii_lowercase();
                    Ok(self
                        .info
                        .headers
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, v)| Value::str(v))
                        .unwrap_or(Value::Nil))
                }
                other => Err(RuntimeError::new(format!(
                    "response has no method `{other}`{}",
                    did_you_mean(other, &["body", "json", "header"])
                ))),
            }
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        Some(match name {
            "url" => Value::str(&self.info.url),
            "status" => Value::Int(self.info.status),
            "status_text" => Value::str(&self.info.status_text),
            "ok" => Value::Bool((200..300).contains(&self.info.status)),
            "headers" => headers_value(&self.info.headers),
            "mime_type" => Value::str(&self.info.mime_type),
            "resource_type" => Value::str(&self.info.resource_type),
            "from_cache" => Value::Bool(self.info.from_cache),
            "request_id" => Value::str(&self.info.request_id),
            "page" => PageObject::bound(self.rt.clone(), self.browser.clone(), self.page.clone()),
            _ => return None,
        })
    }

    fn to_json(&self) -> Option<serde_json::Value> {
        Some(self.json())
    }
}

/// The `event` of `intercept(pattern):`.
pub struct InterceptObject {
    rt: Rc<Runtime>,
    browser: Rc<Browser>,
    page: Page,
    req: Rc<InterceptedRequest>,
}

impl InterceptObject {
    /// Wrap a paused request.
    pub fn value(
        rt: &Rc<Runtime>,
        browser: &Rc<Browser>,
        page: &Page,
        req: Rc<InterceptedRequest>,
    ) -> Value {
        Value::native(InterceptObject {
            rt: rt.clone(),
            browser: browser.clone(),
            page: page.clone(),
            req,
        })
    }
}

impl NativeObject for InterceptObject {
    fn type_name(&self) -> &str {
        "request"
    }

    fn call_method<'a>(
        &'a self,
        _vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            let f = name.as_str();
            match f {
                "continue" => {
                    args.check_kwargs(f, &["url", "method", "headers", "post_data"])?;
                    let o = ContinueOverrides {
                        url: args.kw("url").filter(|v| !v.is_nil()).map(Value::to_string),
                        method: args
                            .kw("method")
                            .filter(|v| !v.is_nil())
                            .map(Value::to_string),
                        headers: match args.kw("headers") {
                            None | Some(Value::Nil) => None,
                            Some(v) => Some(headers_from_value(v, f)?),
                        },
                        post_data: args
                            .kw("post_data")
                            .filter(|v| !v.is_nil())
                            .map(|v| v.to_string().into_bytes()),
                    };
                    self.req
                        .continue_request(o)
                        .await
                        .map_err(|e| convert(e, "continue"))?;
                    Ok(Value::Nil)
                }
                "fulfil" | "fulfill" => {
                    args.check_kwargs(f, &["status", "headers", "body", "content_type"])?;
                    let status = match args.kw("status") {
                        None | Some(Value::Nil) => 200,
                        Some(v) => v.as_int().ok_or_else(|| {
                            RuntimeError::new(format!("{f}: `status:` must be an integer"))
                        })?,
                    };
                    let mut headers = match args.kw("headers") {
                        None | Some(Value::Nil) => vec![],
                        Some(v) => headers_from_value(v, f)?,
                    };
                    let body: Vec<u8> = match args.kw("body").or(args.get(0)) {
                        None | Some(Value::Nil) => vec![],
                        Some(Value::Str(s)) => s.as_bytes().to_vec(),
                        Some(v @ (Value::Map(_) | Value::List(_))) => {
                            if !headers
                                .iter()
                                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
                            {
                                headers.push(("content-type".into(), "application/json".into()));
                            }
                            v.to_json().to_string().into_bytes()
                        }
                        Some(v) => v.to_string().into_bytes(),
                    };
                    if let Some(ct) = args.kw("content_type").filter(|v| !v.is_nil()) {
                        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
                        headers.push(("content-type".into(), ct.to_string()));
                    }
                    self.req
                        .fulfil(status, headers, &body)
                        .await
                        .map_err(|e| convert(e, "fulfil"))?;
                    Ok(Value::Nil)
                }
                "fail" => {
                    args.check_kwargs(f, &["reason"])?;
                    let reason = args
                        .kw("reason")
                        .or(args.get(0))
                        .filter(|v| !v.is_nil())
                        .map(Value::to_string)
                        .unwrap_or_else(|| "Failed".into());
                    self.req
                        .fail(&reason)
                        .await
                        .map_err(|e| convert(e, "fail"))?;
                    Ok(Value::Nil)
                }
                "header" => {
                    args.check_kwargs(f, &[])?;
                    let key = args.require(0, f)?.to_string().to_ascii_lowercase();
                    Ok(self
                        .req
                        .request
                        .headers
                        .iter()
                        .find(|(k, _)| *k == key)
                        .map(|(_, v)| Value::str(v))
                        .unwrap_or(Value::Nil))
                }
                other => Err(RuntimeError::new(format!(
                    "request has no method `{other}`{}",
                    did_you_mean(other, &["continue", "fulfil", "fail", "header"])
                ))),
            }
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        let r = &self.req.request;
        Some(match name {
            "url" => Value::str(&r.url),
            "method" => Value::str(&r.method),
            "headers" => headers_value(&r.headers),
            "resource_type" => Value::str(&r.resource_type),
            "post_data" => r.post_data.as_deref().map(Value::str).unwrap_or(Value::Nil),
            "request_id" => Value::str(&r.request_id),
            "decided" => Value::Bool(self.req.is_decided()),
            "page" => PageObject::bound(self.rt.clone(), self.browser.clone(), self.page.clone()),
            _ => return None,
        })
    }

    fn to_json(&self) -> Option<serde_json::Value> {
        let r = &self.req.request;
        let headers: serde_json::Map<String, serde_json::Value> = r
            .headers
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect();
        Some(serde_json::json!({
            "url": r.url,
            "method": r.method,
            "headers": headers,
            "resource_type": r.resource_type,
            "post_data": r.post_data,
        }))
    }
}

/// The `event` of `on dialog:`.
pub struct DialogObject {
    page: Page,
    kind: String,
    message: String,
    default_prompt: Option<String>,
    url: String,
    decided: Cell<bool>,
}

impl DialogObject {
    /// A dialog that is open and waiting for an answer.
    pub fn new(
        page: Page,
        kind: String,
        message: String,
        default_prompt: Option<String>,
        url: String,
    ) -> DialogObject {
        DialogObject {
            page,
            kind,
            message,
            default_prompt,
            url,
            decided: Cell::new(false),
        }
    }

    /// Wrap as a script value.
    pub fn value(rt: &Rc<Runtime>, browser: &Rc<Browser>, dialog: Rc<DialogObject>) -> Value {
        Value::native(DialogRef {
            rt: rt.clone(),
            browser: browser.clone(),
            dialog,
        })
    }

    /// Whether `accept` / `dismiss` was called.
    pub fn is_decided(&self) -> bool {
        self.decided.get()
    }

    /// Accept (a prompt with `text`, or its default text).
    pub async fn accept(&self, text: Option<String>) -> Result<(), RuntimeError> {
        if self.decided.replace(true) {
            return Err(RuntimeError::new("this dialog was already answered"));
        }
        let prompt = if self.kind == "prompt" {
            Some(text.unwrap_or_else(|| self.default_prompt.clone().unwrap_or_default()))
        } else {
            None
        };
        self.page
            .answer_dialog(true, prompt)
            .await
            .map_err(|e| convert(e, "accept"))
    }

    /// Dismiss / cancel.
    pub async fn dismiss(&self) -> Result<(), RuntimeError> {
        if self.decided.replace(true) {
            return Err(RuntimeError::new("this dialog was already answered"));
        }
        self.page
            .answer_dialog(false, None)
            .await
            .map_err(|e| convert(e, "dismiss"))
    }
}

struct DialogRef {
    rt: Rc<Runtime>,
    browser: Rc<Browser>,
    dialog: Rc<DialogObject>,
}

impl NativeObject for DialogRef {
    fn type_name(&self) -> &str {
        "dialog"
    }

    fn call_method<'a>(
        &'a self,
        _vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            match name.as_str() {
                "accept" => {
                    args.check_kwargs("accept", &[])?;
                    let text = args.get(0).filter(|v| !v.is_nil()).map(Value::to_string);
                    self.dialog.accept(text).await?;
                    Ok(Value::Nil)
                }
                "dismiss" => {
                    args.check_kwargs("dismiss", &[])?;
                    self.dialog.dismiss().await?;
                    Ok(Value::Nil)
                }
                other => Err(RuntimeError::new(format!(
                    "dialog has no method `{other}`{}",
                    did_you_mean(other, &["accept", "dismiss"])
                ))),
            }
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        let d = &self.dialog;
        Some(match name {
            "kind" | "type" => Value::str(&d.kind),
            "message" => Value::str(&d.message),
            "default_prompt" => d
                .default_prompt
                .as_deref()
                .map(Value::str)
                .unwrap_or(Value::Nil),
            "url" => Value::str(&d.url),
            "decided" => Value::Bool(d.is_decided()),
            "page" => PageObject::bound(self.rt.clone(), self.browser.clone(), d.page.clone()),
            _ => return None,
        })
    }

    fn to_json(&self) -> Option<serde_json::Value> {
        let d = &self.dialog;
        Some(serde_json::json!({
            "kind": d.kind,
            "message": d.message,
            "default_prompt": d.default_prompt,
            "url": d.url,
        }))
    }
}
