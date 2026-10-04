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

use crate::browsers::BrowserSlot;
use crate::host::Runtime;
use crate::methods::{did_you_mean, element_method, page_method, PAGE_METHODS};
use futures::future::LocalBoxFuture;
use std::any::Any;
use std::rc::Rc;
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
                    self.rt.instrument(&browser);
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
