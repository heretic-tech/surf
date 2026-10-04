//! The script-facing action surface: one dispatcher for page methods
//! (shared by bare actions, `page.…`, `page(n).…` and `browser.…`) and one
//! for element methods (`all(sel)[0].click()`, the `event` of
//! `on element_appears`). Argument shapes follow `docs/language.md` § 5.2;
//! every browser failure goes through [`crate::errors::convert`] so the
//! diagnostic carries the selector and CDP method.

use crate::errors::convert;
use crate::host::Runtime;
use crate::objects::ElementObject;
use indexmap::IndexMap;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;
use surf_browser::{ActionOptions, Browser, Cookie, DialogPolicy, Element, Page, WaitUntil};
use surf_vm::{Args, Closure, RuntimeError, Value};

/// Keyword arguments every selector action accepts.
const ACTION_KWARGS: &[&str] = &["timeout"];

/// Page methods, in the order the docs list them (`page.help()` shows it).
pub const PAGE_METHODS: &[&str] = &[
    "goto",
    "click",
    "dblclick",
    "right_click",
    "type",
    "fill",
    "press",
    "hover",
    "focus",
    "check",
    "uncheck",
    "select",
    "scroll",
    "scroll_to",
    "text",
    "html",
    "attr",
    "value",
    "exists",
    "count",
    "all",
    "first",
    "wait",
    "wait_gone",
    "wait_text",
    "wait_url",
    "wait_navigation",
    "eval",
    "screenshot",
    "pdf",
    "url",
    "title",
    "back",
    "forward",
    "reload",
    "cookies",
    "set_cookie",
    "set_cookies",
    "clear_cookies",
    "viewport",
    "on_dialog",
    "dialogs",
    "close",
];

/// Element methods.
pub const ELEMENT_METHODS: &[&str] = &[
    "click", "dblclick", "hover", "focus", "type", "fill", "press", "check", "uncheck", "select",
    "scroll", "text", "html", "attr", "value", "visible", "eval", "all", "first",
];

// ───────────────────────── argument helpers ─────────────────────────

fn string(args: &Args, i: usize, f: &str) -> Result<String, RuntimeError> {
    match args.require(i, f)? {
        Value::Str(s) => Ok(s.to_string()),
        Value::Int(n) => Ok(n.to_string()),
        Value::Float(x) => Ok(x.to_string()),
        other => Err(RuntimeError::new(format!(
            "{f}: argument {} must be a string, got {}",
            i + 1,
            other.type_name()
        ))),
    }
}

fn selector(args: &Args, i: usize, f: &str) -> Result<String, RuntimeError> {
    let s = string(args, i, f)?;
    if s.trim().is_empty() {
        return Err(RuntimeError::new(format!("{f}: the selector is empty")));
    }
    Ok(s)
}

fn number(args: &Args, i: usize, f: &str) -> Result<f64, RuntimeError> {
    let v = args.require(i, f)?;
    v.as_f64().ok_or_else(|| {
        RuntimeError::new(format!(
            "{f}: argument {} must be a number, got {}",
            i + 1,
            v.type_name()
        ))
    })
}

fn duration_of(v: &Value) -> Option<Duration> {
    match v {
        Value::Duration(d) => Some(*d),
        Value::Int(ms) => Some(Duration::from_millis((*ms).max(0) as u64)),
        Value::Float(s) => Some(Duration::from_secs_f64(s.max(0.0))),
        _ => None,
    }
}

fn kw_duration(args: &Args, name: &str, f: &str) -> Result<Option<Duration>, RuntimeError> {
    match args.kw(name) {
        None | Some(Value::Nil) => Ok(None),
        Some(v) => duration_of(v).map(Some).ok_or_else(|| {
            RuntimeError::new(format!(
                "{f}: `{name}:` must be a duration such as 5s, got {}",
                v.type_name()
            ))
        }),
    }
}

fn kw_bool(args: &Args, name: &str, f: &str) -> Result<Option<bool>, RuntimeError> {
    match args.kw(name) {
        None | Some(Value::Nil) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(v) => Err(RuntimeError::new(format!(
            "{f}: `{name}:` must be true or false, got {}",
            v.type_name()
        ))),
    }
}

fn kw_string(args: &Args, name: &str, f: &str) -> Result<Option<String>, RuntimeError> {
    match args.kw(name) {
        None | Some(Value::Nil) => Ok(None),
        Some(Value::Str(s)) => Ok(Some(s.to_string())),
        Some(v) => Err(RuntimeError::new(format!(
            "{f}: `{name}:` must be a string, got {}",
            v.type_name()
        ))),
    }
}

/// `timeout:` (+ `delay:` for `type`) into [`ActionOptions`]; rejects
/// keywords outside `extra`.
fn action_opts(args: &Args, f: &str, extra: &[&str]) -> Result<ActionOptions, RuntimeError> {
    let mut allowed: Vec<&str> = ACTION_KWARGS.to_vec();
    allowed.extend_from_slice(extra);
    args.check_kwargs(f, &allowed)?;
    Ok(ActionOptions {
        timeout: kw_duration(args, "timeout", f)?,
        delay: kw_duration(args, "delay", f)?,
    })
}

fn wait_until(args: &Args, f: &str) -> Result<WaitUntil, RuntimeError> {
    match kw_string(args, "wait_until", f)? {
        None => Ok(WaitUntil::Load),
        Some(s) => WaitUntil::parse(&s).ok_or_else(|| {
            RuntimeError::new(format!(
                "{f}: wait_until must be \"load\", \"domcontentloaded\", \"networkidle\" or \"commit\", got {s:?}"
            ))
        }),
    }
}

fn str_list(v: &Value, f: &str) -> Result<Vec<String>, RuntimeError> {
    match v {
        Value::Str(s) => Ok(vec![s.to_string()]),
        Value::Int(n) => Ok(vec![n.to_string()]),
        Value::List(l) => Ok(l.borrow().iter().map(|v| v.to_string()).collect()),
        other => Err(RuntimeError::new(format!(
            "{f}: expected a value or a list of values, got {}",
            other.type_name()
        ))),
    }
}

fn opt_str(v: Option<String>) -> Value {
    v.map(Value::str).unwrap_or(Value::Nil)
}

/// Run `fut` with the page's default timeout temporarily replaced by the
/// call's `timeout:` (navigation waits read the page timeout).
async fn with_timeout<T>(
    page: &Page,
    timeout: Option<Duration>,
    fut: impl std::future::Future<Output = T>,
) -> T {
    let saved = page.timeout();
    if let Some(t) = timeout {
        page.set_timeout(t);
    }
    let r = fut.await;
    page.set_timeout(saved);
    r
}

// ───────────────────────── cookies ─────────────────────────

fn cookie_to_value(c: &Cookie) -> Value {
    let mut m: IndexMap<Rc<str>, Value> = IndexMap::new();
    m.insert("name".into(), Value::str(&c.name));
    m.insert("value".into(), Value::str(&c.value));
    m.insert("domain".into(), opt_str(c.domain.clone()));
    m.insert("path".into(), opt_str(c.path.clone()));
    m.insert(
        "expires".into(),
        c.expires.map(Value::Float).unwrap_or(Value::Nil),
    );
    m.insert(
        "http_only".into(),
        Value::Bool(c.http_only.unwrap_or(false)),
    );
    m.insert("secure".into(), Value::Bool(c.secure.unwrap_or(false)));
    m.insert("same_site".into(), opt_str(c.same_site.clone()));
    Value::map(m)
}

fn cookie_from_value(v: &Value, f: &str) -> Result<Cookie, RuntimeError> {
    let Value::Map(m) = v else {
        return Err(RuntimeError::new(format!(
            "{f}: expected a cookie map {{name, value, domain | url, …}}, got {}",
            v.type_name()
        )));
    };
    let m = m.borrow();
    let get = |keys: &[&str]| -> Option<Value> {
        keys.iter()
            .find_map(|k| m.get(*k).cloned())
            .filter(|v| !v.is_nil())
    };
    let name = get(&["name"])
        .map(|v| v.to_string())
        .ok_or_else(|| RuntimeError::new(format!("{f}: cookie needs a `name`")))?;
    let value = get(&["value"]).map(|v| v.to_string()).unwrap_or_default();
    Ok(Cookie {
        name,
        value,
        domain: get(&["domain"]).map(|v| v.to_string()),
        path: get(&["path"]).map(|v| v.to_string()),
        expires: get(&["expires"]).and_then(|v| v.as_f64()),
        http_only: get(&["http_only", "httpOnly"]).map(|v| v.is_truthy()),
        secure: get(&["secure"]).map(|v| v.is_truthy()),
        same_site: get(&["same_site", "sameSite"]).map(|v| v.to_string()),
        url: get(&["url"]).map(|v| v.to_string()),
    })
}

// ───────────────────────── eval(fn(): …) ─────────────────────────

/// The JavaScript text of a zero-argument lambda: its body, verbatim
/// (`docs/language.md` § 5.2). A block lambda becomes an immediately
/// invoked arrow function.
pub fn lambda_js(rt: &Runtime, f: &Closure) -> Result<String, RuntimeError> {
    if !f.func.params.is_empty() {
        return Err(RuntimeError::new(
            "eval(fn): the lambda takes no parameters — use eval(fn(): expr)",
        ));
    }
    let source = rt.source();
    let text = f.func.span.slice(&source.text);
    let rest = text.trim_start();
    let Some(rest) = rest.strip_prefix("fn") else {
        return Err(RuntimeError::new(
            "eval(fn): cannot recover the lambda's source text",
        ));
    };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix('(') else {
        return Err(RuntimeError::new(
            "eval(fn): cannot recover the lambda's source text",
        ));
    };
    let Some(close) = rest.find(')') else {
        return Err(RuntimeError::new(
            "eval(fn): cannot recover the lambda's source text",
        ));
    };
    let rest = rest[close + 1..].trim_start();
    let Some(body) = rest.strip_prefix(':') else {
        return Err(RuntimeError::new(
            "eval(fn): cannot recover the lambda's source text",
        ));
    };
    let first_line_blank = body
        .split('\n')
        .next()
        .map(|l| l.trim().is_empty() || l.trim_start().starts_with('#'))
        .unwrap_or(true);
    if first_line_blank {
        // Block lambda: strip the common indentation and run as a function
        // body so `return` works.
        let lines: Vec<&str> = body.lines().filter(|l| !l.trim().is_empty()).collect();
        let indent = lines
            .iter()
            .map(|l| l.len() - l.trim_start().len())
            .min()
            .unwrap_or(0);
        let dedented: Vec<&str> = lines
            .iter()
            .map(|l| {
                if l.len() >= indent {
                    &l[indent..]
                } else {
                    l.trim_start()
                }
            })
            .collect();
        Ok(format!("(() => {{\n{}\n}})()", dedented.join("\n")))
    } else {
        Ok(body.trim().to_owned())
    }
}

// ───────────────────────── page methods ─────────────────────────

/// Dispatch `page.<name>(args)` (also the bare action `<name>(args)`).
pub async fn page_method(
    rt: &Runtime,
    browser: &Rc<Browser>,
    page: &Page,
    name: &str,
    args: Args,
) -> Result<Value, RuntimeError> {
    let f = name;
    let err = |e| convert(e, f);
    match name {
        "goto" => {
            args.check_kwargs(f, &["wait_until", "timeout"])?;
            let url = string(&args, 0, f)?;
            let wu = wait_until(&args, f)?;
            let timeout = kw_duration(&args, "timeout", f)?;
            with_timeout(page, timeout, page.goto(&url, wu))
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "click" => {
            let opts = action_opts(&args, f, &["button", "count"])?;
            let sel = selector(&args, 0, f)?;
            let button = kw_string(&args, "button", f)?.unwrap_or_else(|| "left".into());
            let count = args.kw("count").and_then(Value::as_int).unwrap_or(1);
            match (button.as_str(), count) {
                ("left", 1) => page.click(&sel, opts).await,
                ("left", 2) => page.dblclick(&sel, opts).await,
                ("right", _) => page.right_click(&sel, opts).await,
                (b, c) => {
                    return Err(RuntimeError::new(format!(
                        "click: unsupported button {b:?} / count {c} (left ×1, left ×2, right)"
                    ))
                    .with_selector(sel))
                }
            }
            .map_err(err)?;
            Ok(Value::Nil)
        }
        "dblclick" => {
            let opts = action_opts(&args, f, &[])?;
            page.dblclick(&selector(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "right_click" => {
            let opts = action_opts(&args, f, &[])?;
            page.right_click(&selector(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "type" => {
            let opts = action_opts(&args, f, &["delay"])?;
            let sel = selector(&args, 0, f)?;
            let text = string(&args, 1, f)?;
            page.type_text(&sel, &text, opts).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "fill" => {
            let opts = action_opts(&args, f, &[])?;
            let sel = selector(&args, 0, f)?;
            let text = string(&args, 1, f)?;
            page.fill(&sel, &text, opts).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "press" => {
            let opts = action_opts(&args, f, &[])?;
            if args.positional.len() >= 2 {
                let sel = selector(&args, 0, f)?;
                let key = string(&args, 1, f)?;
                page.press_on(&sel, &key, opts).await.map_err(err)?;
            } else {
                let key = string(&args, 0, f)?;
                page.press(&key).await.map_err(err)?;
            }
            Ok(Value::Nil)
        }
        "hover" => {
            let opts = action_opts(&args, f, &[])?;
            page.hover(&selector(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "focus" => {
            let opts = action_opts(&args, f, &[])?;
            page.focus(&selector(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "check" => {
            let opts = action_opts(&args, f, &[])?;
            page.check(&selector(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "uncheck" => {
            let opts = action_opts(&args, f, &[])?;
            page.uncheck(&selector(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "select" => {
            let opts = action_opts(&args, f, &[])?;
            let sel = selector(&args, 0, f)?;
            let values = str_list(args.require(1, f)?, f)?;
            let refs: Vec<&str> = values.iter().map(String::as_str).collect();
            let chosen = page.select(&sel, &refs, opts).await.map_err(err)?;
            Ok(Value::list(chosen.into_iter().map(Value::str).collect()))
        }
        "scroll" => {
            if matches!(args.get(0), Some(Value::Str(_))) {
                let opts = action_opts(&args, f, &[])?;
                page.scroll_into_view(&selector(&args, 0, f)?, opts)
                    .await
                    .map_err(err)?;
            } else {
                args.check_kwargs(f, &[])?;
                let x = number(&args, 0, f)?;
                let y = number(&args, 1, f)?;
                page.scroll_by(x, y).await.map_err(err)?;
            }
            Ok(Value::Nil)
        }
        "scroll_to" => {
            args.check_kwargs(f, &[])?;
            let x = number(&args, 0, f)?;
            let y = number(&args, 1, f)?;
            page.scroll_to(x, y).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "text" => {
            let opts = action_opts(&args, f, &[])?;
            let s = page
                .text(&selector(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::str(s))
        }
        "html" => {
            let opts = action_opts(&args, f, &[])?;
            let s = page
                .html(&selector(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::str(s))
        }
        "attr" => {
            let opts = action_opts(&args, f, &[])?;
            let sel = selector(&args, 0, f)?;
            let attr = string(&args, 1, f)?;
            let v = page.attr(&sel, &attr, opts).await.map_err(err)?;
            Ok(opt_str(v))
        }
        "value" => {
            let opts = action_opts(&args, f, &[])?;
            let s = page
                .value(&selector(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::str(s))
        }
        "exists" => {
            args.check_kwargs(f, &[])?;
            let b = page.exists(&selector(&args, 0, f)?).await.map_err(err)?;
            Ok(Value::Bool(b))
        }
        "count" => {
            args.check_kwargs(f, &[])?;
            let n = page.count(&selector(&args, 0, f)?).await.map_err(err)?;
            Ok(Value::Int(n as i64))
        }
        "all" => {
            args.check_kwargs(f, &[])?;
            let els = page.all(&selector(&args, 0, f)?).await.map_err(err)?;
            Ok(Value::list(
                els.into_iter()
                    .map(|el| ElementObject::value(rt, browser, page, el))
                    .collect(),
            ))
        }
        "first" => {
            args.check_kwargs(f, &[])?;
            let el = page.first(&selector(&args, 0, f)?).await.map_err(err)?;
            Ok(el
                .map(|el| ElementObject::value(rt, browser, page, el))
                .unwrap_or(Value::Nil))
        }
        "wait" => {
            if let Some(d) = args.get(0).and_then(duration_of) {
                args.check_kwargs(f, &[])?;
                tokio::time::sleep(d).await;
                return Ok(Value::Nil);
            }
            let opts = action_opts(&args, f, &[])?;
            let el = page
                .wait(&selector(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(ElementObject::value(rt, browser, page, el))
        }
        "wait_gone" => {
            let opts = action_opts(&args, f, &[])?;
            page.wait_gone(&selector(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "wait_text" => {
            let opts = action_opts(&args, f, &[])?;
            let sel = selector(&args, 0, f)?;
            let needle = string(&args, 1, f)?;
            page.wait_text(&sel, &needle, opts).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "wait_url" => {
            let opts = action_opts(&args, f, &[])?;
            page.wait_url(&string(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "wait_navigation" => {
            args.check_kwargs(f, &["wait_until", "timeout"])?;
            let wu = wait_until(&args, f)?;
            let timeout = kw_duration(&args, "timeout", f)?;
            with_timeout(page, timeout, page.wait_for_navigation(wu))
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "eval" => {
            args.check_kwargs(f, &[])?;
            let js = match args.require(0, f)? {
                Value::Str(s) => s.to_string(),
                Value::Fn(c) => lambda_js(rt, c)?,
                other => {
                    return Err(RuntimeError::new(format!(
                        "eval: expected JavaScript text or fn(): expr, got {}",
                        other.type_name()
                    )))
                }
            };
            let v = page.eval(&js).await.map_err(err)?;
            Ok(Value::from_json(v))
        }
        "screenshot" => {
            args.check_kwargs(f, &["full"])?;
            let path = string(&args, 0, f)?;
            let full = kw_bool(&args, "full", f)?.unwrap_or(false);
            page.screenshot(Path::new(&path), full).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "pdf" => {
            args.check_kwargs(f, &[])?;
            let path = string(&args, 0, f)?;
            page.pdf(Path::new(&path)).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "url" => {
            args.check_kwargs(f, &[])?;
            Ok(Value::str(page.url().await.map_err(err)?))
        }
        "title" => {
            args.check_kwargs(f, &[])?;
            Ok(Value::str(page.title().await.map_err(err)?))
        }
        "back" | "forward" => {
            args.check_kwargs(f, &["wait_until", "timeout"])?;
            let wu = wait_until(&args, f)?;
            let timeout = kw_duration(&args, "timeout", f)?;
            let moved = if name == "back" {
                with_timeout(page, timeout, page.back(wu)).await
            } else {
                with_timeout(page, timeout, page.forward(wu)).await
            }
            .map_err(err)?;
            Ok(Value::Bool(moved))
        }
        "reload" => {
            args.check_kwargs(f, &["wait_until", "timeout"])?;
            let wu = wait_until(&args, f)?;
            let timeout = kw_duration(&args, "timeout", f)?;
            with_timeout(page, timeout, page.reload(wu))
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "cookies" => {
            args.check_kwargs(f, &[])?;
            let cookies = page.cookies().await.map_err(err)?;
            Ok(Value::list(cookies.iter().map(cookie_to_value).collect()))
        }
        "set_cookie" => {
            args.check_kwargs(f, &[])?;
            let mut cookie = if args.positional.len() >= 2 {
                let name = string(&args, 0, f)?;
                let value = string(&args, 1, f)?;
                Cookie {
                    name,
                    value,
                    ..Default::default()
                }
            } else {
                cookie_from_value(args.require(0, f)?, f)?
            };
            if cookie.domain.is_none() && cookie.url.is_none() {
                cookie.url = Some(page.url().await.map_err(err)?);
            }
            page.set_cookies(&[cookie]).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "set_cookies" => {
            args.check_kwargs(f, &[])?;
            let Value::List(list) = args.require(0, f)? else {
                return Err(RuntimeError::new(
                    "set_cookies: expected a list of cookie maps",
                ));
            };
            let items: Vec<Value> = list.borrow().clone();
            let mut cookies = Vec::with_capacity(items.len());
            for v in &items {
                cookies.push(cookie_from_value(v, f)?);
            }
            page.set_cookies(&cookies).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "clear_cookies" => {
            args.check_kwargs(f, &[])?;
            page.clear_cookies().await.map_err(err)?;
            Ok(Value::Nil)
        }
        "viewport" => {
            args.check_kwargs(f, &[])?;
            let w = number(&args, 0, f)? as u32;
            let h = number(&args, 1, f)? as u32;
            page.set_viewport(w, h).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "on_dialog" => {
            args.check_kwargs(f, &[])?;
            let policy = match string(&args, 0, f)?.as_str() {
                "accept" => DialogPolicy::Accept,
                "dismiss" => DialogPolicy::Dismiss,
                text => DialogPolicy::AcceptWith(text.to_owned()),
            };
            page.on_dialog(policy);
            Ok(Value::Nil)
        }
        "dialogs" => {
            args.check_kwargs(f, &[])?;
            Ok(Value::list(
                page.dialogs()
                    .into_iter()
                    .map(|d| {
                        let mut m: IndexMap<Rc<str>, Value> = IndexMap::new();
                        m.insert("kind".into(), Value::str(d.kind));
                        m.insert("message".into(), Value::str(d.message));
                        m.insert("default_prompt".into(), opt_str(d.default_prompt));
                        m.insert("url".into(), Value::str(d.url));
                        Value::map(m)
                    })
                    .collect(),
            ))
        }
        "close" => {
            args.check_kwargs(f, &[])?;
            browser.close_page(page).await.map_err(err)?;
            Ok(Value::Nil)
        }
        _ => Err(RuntimeError::new(format!(
            "page has no method `{name}`{}",
            did_you_mean(name, PAGE_METHODS)
        ))),
    }
}

// ───────────────────────── element methods ─────────────────────────

/// Dispatch `element.<name>(args)`.
pub async fn element_method(
    rt: &Runtime,
    browser: &Rc<Browser>,
    page: &Page,
    el: &Element,
    name: &str,
    args: Args,
) -> Result<Value, RuntimeError> {
    let f = name;
    let err = |e| convert(e, f);
    match name {
        "click" => {
            let opts = action_opts(&args, f, &["count"])?;
            if args.kw("count").and_then(Value::as_int) == Some(2) {
                el.dblclick(opts).await.map_err(err)?;
            } else {
                el.click(opts).await.map_err(err)?;
            }
            Ok(Value::Nil)
        }
        "dblclick" => {
            el.dblclick(action_opts(&args, f, &[])?)
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "hover" => {
            el.hover(action_opts(&args, f, &[])?).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "focus" => {
            args.check_kwargs(f, &[])?;
            el.focus().await.map_err(err)?;
            Ok(Value::Nil)
        }
        "type" => {
            let opts = action_opts(&args, f, &["delay"])?;
            el.type_text(&string(&args, 0, f)?, opts)
                .await
                .map_err(err)?;
            Ok(Value::Nil)
        }
        "fill" => {
            let opts = action_opts(&args, f, &[])?;
            el.fill(&string(&args, 0, f)?, opts).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "press" => {
            let opts = action_opts(&args, f, &[])?;
            el.press(&string(&args, 0, f)?, opts).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "check" => {
            el.check(action_opts(&args, f, &[])?).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "uncheck" => {
            el.uncheck(action_opts(&args, f, &[])?).await.map_err(err)?;
            Ok(Value::Nil)
        }
        "select" => {
            let opts = action_opts(&args, f, &[])?;
            let values = str_list(args.require(0, f)?, f)?;
            let refs: Vec<&str> = values.iter().map(String::as_str).collect();
            let chosen = el.select(&refs, opts).await.map_err(err)?;
            Ok(Value::list(chosen.into_iter().map(Value::str).collect()))
        }
        "scroll" => {
            args.check_kwargs(f, &[])?;
            el.scroll_into_view().await.map_err(err)?;
            Ok(Value::Nil)
        }
        "text" => {
            args.check_kwargs(f, &[])?;
            Ok(Value::str(el.text().await.map_err(err)?))
        }
        "html" => {
            args.check_kwargs(f, &[])?;
            Ok(Value::str(el.html().await.map_err(err)?))
        }
        "attr" => {
            args.check_kwargs(f, &[])?;
            Ok(opt_str(el.attr(&string(&args, 0, f)?).await.map_err(err)?))
        }
        "value" => {
            args.check_kwargs(f, &[])?;
            Ok(Value::str(el.value().await.map_err(err)?))
        }
        "visible" => {
            args.check_kwargs(f, &[])?;
            Ok(Value::Bool(el.is_visible().await.map_err(err)?))
        }
        "eval" => {
            args.check_kwargs(f, &[])?;
            let js = match args.require(0, f)? {
                Value::Str(s) => s.to_string(),
                Value::Fn(c) => lambda_js(rt, c)?,
                other => {
                    return Err(RuntimeError::new(format!(
                        "eval: expected JavaScript text or fn(): expr, got {}",
                        other.type_name()
                    )))
                }
            };
            // `this` is the element; the text is an expression.
            let decl = format!("function() {{ return ({js}); }}");
            let v = el.eval_fn(&decl, vec![]).await.map_err(err)?;
            Ok(Value::from_json(v))
        }
        "all" => {
            args.check_kwargs(f, &[])?;
            let els = el.all(&selector(&args, 0, f)?).await.map_err(err)?;
            Ok(Value::list(
                els.into_iter()
                    .map(|e| ElementObject::value(rt, browser, page, e))
                    .collect(),
            ))
        }
        "first" => {
            args.check_kwargs(f, &[])?;
            let mut els = el.all(&selector(&args, 0, f)?).await.map_err(err)?;
            if els.is_empty() {
                Ok(Value::Nil)
            } else {
                Ok(ElementObject::value(rt, browser, page, els.remove(0)))
            }
        }
        _ => Err(RuntimeError::new(format!(
            "element has no method `{name}`{}",
            did_you_mean(name, ELEMENT_METHODS)
        ))),
    }
}

/// ` — did you mean `x`?` or an empty string.
pub fn did_you_mean(name: &str, candidates: &[&str]) -> String {
    match surf_syntax::diagnostics::suggest(name, candidates.iter().copied()) {
        Some(s) => format!(" — did you mean `{s}`?"),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::RuntimeOptions;
    use surf_syntax::Span;
    use surf_vm::bytecode::Function;

    fn lambda(source: &str, span: Span) -> (Rc<Runtime>, Closure) {
        let rt = Runtime::new(RuntimeOptions::default());
        rt.set_source("t.surf", source);
        let func = Rc::new(Function {
            name: "<lambda>".into(),
            params: vec![],
            has_default: vec![],
            locals: 0,
            upvalues: vec![],
            chunk: Default::default(),
            span,
            line_starts: Rc::from(vec![]),
        });
        (rt, Closure::new(func))
    }

    #[test]
    fn expression_lambda_is_verbatim() {
        let src = "x = eval(fn(): document.title + '!')\n";
        let (rt, c) = lambda(src, Span::new(9, 35));
        assert_eq!(lambda_js(&rt, &c).unwrap(), "document.title + '!'");
    }

    #[test]
    fn block_lambda_becomes_an_iife() {
        let src = "x = eval(fn():\n    const a = 1\n    return a + 1\n)\nprint(x)\n";
        let (rt, c) = lambda(src, Span::new(9, 47));
        assert_eq!(
            lambda_js(&rt, &c).unwrap(),
            "(() => {\nconst a = 1\nreturn a + 1\n})()"
        );
    }

    #[test]
    fn cookie_round_trip() {
        let c = Cookie {
            name: "a".into(),
            value: "b".into(),
            domain: Some("x.test".into()),
            path: Some("/".into()),
            expires: None,
            http_only: Some(true),
            secure: Some(false),
            same_site: Some("Lax".into()),
            url: None,
        };
        let v = cookie_to_value(&c);
        let back = cookie_from_value(&v, "set_cookie").unwrap();
        assert_eq!(back.name, "a");
        assert_eq!(back.domain.as_deref(), Some("x.test"));
        assert_eq!(back.http_only, Some(true));
        assert_eq!(back.same_site.as_deref(), Some("Lax"));
    }

    #[test]
    fn suggestions() {
        assert_eq!(
            did_you_mean("clik", PAGE_METHODS),
            " — did you mean `click`?"
        );
    }
}
