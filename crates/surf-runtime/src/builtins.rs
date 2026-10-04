//! Bare-action globals and the other host-provided names.
//!
//! The VM resolves a bare name in this order: `Host::resolve_global`
//! (user `fn`s, `page`, `browser`, named browsers, `self`, supervisors,
//! bare actions) → stdlib `Globals` → `Host::call_global` (everything
//! else: `task` calls, `shift_proxy`, `send` / `broadcast` / `receive` /
//! `wait_for_message`, and the "undefined function" error with a
//! suggestion).
//! Bare actions are returned from `resolve_global` as callable values so
//! they shadow stdlib names of the same spelling (`type`); see
//! [`bare_action`] for the one-argument `type(v)` carve-out.

use crate::host::Runtime;
use crate::methods::{did_you_mean, page_method, PAGE_METHODS};
use surf_vm::{Args, RuntimeError, Value, Vm};

/// Bare actions that resolve to the sole page (`docs/language.md` § 5):
/// every page method except `close`.
pub const BARE_ACTIONS: &[&str] = &[
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
];

/// Concurrency builtins provided by the runtime.
const CONCURRENCY: &[&str] = &[
    "shift_proxy",
    "send",
    "broadcast",
    "receive",
    "wait_for_message",
];

/// Static name for a bare action (so `BareAction` values carry `&'static str`).
pub fn bare_action_name(name: &str) -> Option<&'static str> {
    BARE_ACTIONS.iter().copied().find(|n| *n == name)
}

/// Run bare action `name` on the sole page. `type(v)` with a single
/// argument is the stdlib type-of function, since the action needs two.
pub async fn bare_action(
    rt: &Runtime,
    vm: &mut Vm,
    name: &str,
    args: Args,
) -> Result<Value, RuntimeError> {
    if name == "type" && args.positional.len() == 1 && args.kwargs.is_empty() {
        if let Some(f) = vm.globals().get("type") {
            return f(vm, args).await;
        }
    }
    let (browser, page) = rt.sole_page(name).await?;
    page_method(rt, &browser, &page, name, args).await
}

/// `Host::call_global`: names the VM could not resolve anywhere else.
pub async fn call(
    rt: &Runtime,
    vm: &mut Vm,
    name: &str,
    args: Args,
) -> Result<Value, RuntimeError> {
    if let Some(f) = rt.user_fn(name) {
        return vm.call(f, args).await;
    }
    if let Some(f) = vm.globals().get(name) {
        return f(vm, args).await;
    }
    if PAGE_METHODS.contains(&name) {
        return bare_action(rt, vm, name, args).await;
    }
    match name {
        "shift_proxy" => return crate::tasks::shift_proxy(&rt.rc(), args).await,
        "send" => return crate::actors::send(&rt.rc(), args),
        "broadcast" => return crate::actors::broadcast(&rt.rc(), args),
        "receive" | "wait_for_message" => return crate::actors::receive(name, args).await,
        _ => {}
    }
    if let Some(decl) = rt.task_decl(name) {
        return crate::tasks::call_decl(&rt.rc(), vm, decl, args).await;
    }
    if rt.supervisor(name).is_some() {
        return Err(RuntimeError::new(format!(
            "supervisor {name} is not callable — it starts by itself; use {name}.join() or {name}.stop()"
        )));
    }
    let mut candidates: Vec<&str> = BARE_ACTIONS.to_vec();
    candidates.extend(["page", "browser", "print", "sleep", "env", "emit", "exit"]);
    candidates.extend(CONCURRENCY);
    let user: Vec<String> = rt.callable_names();
    candidates.extend(user.iter().map(String::as_str));
    let globals = vm.globals().names();
    candidates.extend(globals.iter().map(|s| &**s));
    Err(RuntimeError::new(format!(
        "undefined function `{name}`{}",
        did_you_mean(name, &candidates)
    )))
}
