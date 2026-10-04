//! Host-independent builtins.
//!
//! Everything here is pure or routes IO through [`crate::Host`]:
//! `len`, `keys`, `values`, `push`, `join`, `split`, `trim`, `upper`,
//! `lower`, `contains`, `starts_with`, `replace`, `json`, `parse_json`,
//! `int`, `float`, `str`, `type`, `range`, `now`, `random`, `read_file`,
//! `write_file`, `env`, `sleep`, `print`.
//!
//! Browser-facing globals (`goto`, `click`, `page`, …) are **not** here —
//! they come from `Host::call_global`.

use crate::error::RuntimeError;
use crate::value::{Args, Value};
use crate::vm::Vm;
use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;

/// A native (Rust) function callable from scripts.
pub type NativeFn =
    Rc<dyn for<'a> Fn(&'a mut Vm, Args) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>>;

/// Registry of global native functions shared by every VM in a process.
#[derive(Default)]
pub struct Globals {
    fns: RefCell<IndexMap<Rc<str>, NativeFn>>,
}

impl Globals {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry pre-populated with the standard library. Implemented in task 6.
    pub fn stdlib() -> Self {
        Self::new()
    }

    /// Register / replace a function.
    pub fn register(&self, name: &str, f: NativeFn) {
        self.fns.borrow_mut().insert(Rc::from(name), f);
    }

    /// Look up a function.
    pub fn get(&self, name: &str) -> Option<NativeFn> {
        self.fns.borrow().get(name).cloned()
    }

    /// All registered names (for `surf check` undefined-name warnings).
    pub fn names(&self) -> Vec<Rc<str>> {
        self.fns.borrow().keys().cloned().collect()
    }
}
