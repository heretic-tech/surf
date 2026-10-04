//! The [`Host`] trait: everything the VM needs from the outside world.
//!
//! `surf-runtime` implements it on top of `surf-browser`; tests implement a
//! fake. Nothing in this module performs IO.

use crate::error::RuntimeError;
use crate::value::{Args, Closure, Value};
use crate::vm::Vm;
use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use std::rc::Rc;
use std::time::Duration;

/// A `key: value` property on a task / actor / supervisor header.
///
/// Literal props (`retry: 5`, `timeout: 60s`) are `Const`; props that need
/// evaluation at use time (`on_fail: shift_proxy()`) are `Lazy` thunks the
/// host calls when the property is consulted.
#[derive(Clone)]
pub enum Prop {
    /// Already-evaluated literal.
    Const(Value),
    /// Zero-argument closure to evaluate on demand.
    Lazy(Rc<Closure>),
}

impl Prop {
    /// Evaluate the property (calls the thunk for `Lazy`).
    pub async fn resolve(&self, vm: &mut Vm) -> Result<Value, RuntimeError> {
        match self {
            Prop::Const(v) => Ok(v.clone()),
            Prop::Lazy(c) => vm.call(c.clone(), Args::default()).await,
        }
    }
}

/// A hoisted top-level declaration, handed to the host before the first
/// statement of the program executes (in source order).
pub enum Declaration {
    /// `browser:` / `browser work:` block. Props are evaluated eagerly
    /// (`env(...)` is allowed; nothing launches).
    Config {
        /// Block kind — `browser`.
        name: String,
        /// `browser work:` → `Some("work")`.
        alias: Option<String>,
        /// Evaluated properties.
        props: IndexMap<Rc<str>, Value>,
    },
    /// `fn name(…):`
    Fn {
        /// Function name.
        name: String,
        /// Compiled closure.
        closure: Rc<Closure>,
    },
    /// `task name(params):`
    Task {
        /// Task name.
        name: String,
        /// Parameter names.
        params: Vec<Rc<str>>,
        /// `retry`, `on_fail`, `timeout`, `fresh`, …
        props: IndexMap<Rc<str>, Prop>,
        /// Body closure taking `params`.
        body: Rc<Closure>,
    },
    /// `actor Name(params):`
    Actor {
        /// Actor name.
        name: String,
        /// Parameter names.
        params: Vec<Rc<str>>,
        /// Header properties.
        props: IndexMap<Rc<str>, Prop>,
        /// Body closure taking `params`; runs with its own page + mailbox.
        body: Rc<Closure>,
    },
    /// `supervisor Name:`
    Supervisor {
        /// Supervisor name.
        name: String,
        /// `strategy`, `max_restarts`, `within`.
        props: IndexMap<Rc<str>, Prop>,
        /// Body closure; its `spawn` statements define the children.
        body: Rc<Closure>,
    },
    /// `on <event>(<args>):`
    Handler {
        /// Event name (`element_appears`, `navigation`, `dialog`, `request`,
        /// `response`, `message`).
        event: String,
        /// Evaluated handler arguments (selector / URL pattern).
        args: Vec<Value>,
        /// Body closure taking one parameter: the event payload.
        body: Rc<Closure>,
    },
}

/// Filesystem operations requested by the stdlib (`read_file`, …). Routed
/// through the host so `surf-vm` stays free of `std::fs`.
#[derive(Debug, Clone, PartialEq)]
pub enum FsOp {
    /// Read a whole file as a string.
    Read(String),
    /// Overwrite a file.
    Write(String, String),
    /// Append to a file.
    Append(String, String),
    /// Whether a path exists.
    Exists(String),
}

/// What the VM needs from its embedder.
///
/// Single-threaded: implementors use `Rc` / `RefCell` freely. Every method
/// that may block returns a `LocalBoxFuture`.
pub trait Host {
    /// Register a hoisted declaration (called during the program prologue).
    fn declare(&self, decl: Declaration);

    /// Resolve a global name the VM does not know (`page`, `browser`,
    /// `self`, task/actor names, …). `None` → undefined-variable error.
    fn resolve_global(&self, name: &str) -> Option<Value>;

    /// Call a global function the VM does not know — bare actions
    /// (`goto`, `click`, …), `page(n)`, `env(...)`, `shift_proxy()`, tasks.
    fn call_global<'a>(
        &'a self,
        vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;

    /// `spawn name(args)` — start a task / actor / fn on its own tokio task
    /// and return a handle (a `Native` value with `.join()`, `.id`, …).
    fn spawn<'a>(
        &'a self,
        vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;

    /// `parallel for x in items:` — run `body(item)` concurrently, honouring
    /// `limit` / `fail_fast` in `opts`. Resolves when all items finished.
    fn parallel_for<'a>(
        &'a self,
        vm: &'a mut Vm,
        items: Value,
        body: Rc<Closure>,
        opts: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;

    /// `sleep(d)`.
    fn sleep<'a>(&'a self, d: Duration) -> LocalBoxFuture<'a, ()>;

    /// `print(...)` — one line to stdout.
    fn print(&self, s: &str);

    /// `emit v` — one JSON line to stdout.
    fn emit(&self, v: &Value);

    /// Filesystem access for the stdlib.
    fn fs<'a>(&'a self, op: FsOp) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;

    /// `env("NAME")`.
    fn env(&self, name: &str) -> Option<String>;
}
