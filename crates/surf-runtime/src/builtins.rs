//! Bare-action globals and other host functions. Implemented in task 7.
//!
//! Names resolved here (in this order): user `fn`s, tasks / actors (as
//! callables), bare actions (`goto click type fill press hover check select
//! scroll text html attr value exists count all wait wait_gone wait_text
//! wait_url eval screenshot pdf url title back reload cookies set_cookie`),
//! `page(…)`, `browser`, `env`, `sleep`, `shift_proxy`, `send`,
//! `broadcast`, `receive`, `wait_for_message`, then stdlib `Globals`.

use crate::host::Runtime;
use surf_vm::{Args, RuntimeError, Value, Vm};

/// Bare actions that resolve to the sole page.
pub const BARE_ACTIONS: &[&str] = &[
    "goto",
    "click",
    "type",
    "fill",
    "press",
    "hover",
    "check",
    "select",
    "scroll",
    "text",
    "html",
    "attr",
    "value",
    "exists",
    "count",
    "all",
    "wait",
    "wait_gone",
    "wait_text",
    "wait_url",
    "eval",
    "screenshot",
    "pdf",
    "url",
    "title",
    "back",
    "reload",
    "cookies",
    "set_cookie",
];

/// Dispatch a global call.
pub async fn call(
    rt: &Runtime,
    vm: &mut Vm,
    name: &str,
    args: Args,
) -> Result<Value, RuntimeError> {
    if let Some(f) = rt.resolve_global_fn(name) {
        return vm.call(f, args).await;
    }
    if let Some(native) = vm.globals().get(name) {
        return native(vm, args).await;
    }
    if BARE_ACTIONS.contains(&name) {
        let _ = rt.pages().sole(name)?;
        return Err(RuntimeError::new(format!(
            "{name}: bare actions not implemented yet (task 7)"
        )));
    }
    Err(RuntimeError::new(format!("undefined function `{name}`")))
}

impl Runtime {
    /// User-declared `fn` by name.
    pub fn resolve_global_fn(&self, name: &str) -> Option<std::rc::Rc<surf_vm::Closure>> {
        match surf_vm::Host::resolve_global(self, name) {
            Some(Value::Fn(f)) => Some(f),
            _ => None,
        }
    }
}
