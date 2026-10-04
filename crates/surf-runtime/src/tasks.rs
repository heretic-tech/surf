//! `spawn`, `parallel for`, and `task` properties. Implemented in task 8.

use crate::host::Runtime;
use std::rc::Rc;
use surf_vm::{Args, Closure, RuntimeError, Value, Vm};

/// `spawn name(args)`.
pub async fn spawn(
    rt: &Runtime,
    vm: &mut Vm,
    name: &str,
    args: Args,
) -> Result<Value, RuntimeError> {
    let _ = (rt.lifetime().running(), vm.cancel_token(), args);
    Err(RuntimeError::new(format!(
        "spawn {name}: not implemented yet — it arrives with task 8 (concurrency)"
    )))
}

/// `parallel for x in items:` with `limit` / `fail_fast` in `opts`.
pub async fn parallel_for(
    rt: &Runtime,
    vm: &mut Vm,
    items: Value,
    body: Rc<Closure>,
    opts: Args,
) -> Result<Value, RuntimeError> {
    let _ = (
        rt.lifetime().running(),
        vm.cancel_token(),
        items,
        body,
        opts,
    );
    Err(RuntimeError::new(
        "parallel for: not implemented yet — it arrives with task 8 (concurrency)",
    ))
}
