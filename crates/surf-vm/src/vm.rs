//! The interpreter.

use crate::bytecode::CompiledProgram;
use crate::cancel::CancelToken;
use crate::error::RuntimeError;
use crate::host::Host;
use crate::stdlib::Globals;
use crate::value::{Args, Closure, Value};
use std::rc::Rc;

/// One interpreter instance: a value stack and call frames. The runtime
/// creates one `Vm` per concurrent task (spawned task, actor, handler
/// invocation, `parallel for` item); they all share the same `Host` and
/// `Globals`.
pub struct Vm {
    host: Rc<dyn Host>,
    globals: Rc<Globals>,
    cancel: CancelToken,
    stack: Vec<Value>,
}

impl Vm {
    /// Create a VM bound to `host` and `globals`.
    pub fn new(host: Rc<dyn Host>, globals: Rc<Globals>) -> Vm {
        Vm {
            host,
            globals,
            cancel: CancelToken::new(),
            stack: Vec::new(),
        }
    }

    /// Create a VM sharing this one's host, globals and cancel token (used
    /// by the runtime for spawned tasks).
    pub fn fork(&self) -> Vm {
        Vm {
            host: self.host.clone(),
            globals: self.globals.clone(),
            cancel: self.cancel.clone(),
            stack: Vec::new(),
        }
    }

    /// The host.
    pub fn host(&self) -> &Rc<dyn Host> {
        &self.host
    }

    /// The globals registry.
    pub fn globals(&self) -> &Rc<Globals> {
        &self.globals
    }

    /// Run a compiled program to completion (prologue declarations, then
    /// statements). Returns when the main body finishes; the runtime decides
    /// whether to keep the process alive for handlers / tasks.
    pub async fn run(&mut self, program: &CompiledProgram) -> Result<(), RuntimeError> {
        let main = Rc::new(Closure::new(program.main.clone()));
        self.call(main, Args::default()).await.map(|_| ())
    }

    /// Call a closure with arguments. Implemented in task 6.
    pub async fn call(&mut self, f: Rc<Closure>, args: Args) -> Result<Value, RuntimeError> {
        if self.cancel.is_cancelled() {
            return Err(RuntimeError::new("cancelled"));
        }
        self.stack.clear();
        Err(RuntimeError::new(format!(
            "vm not implemented yet (call {} with {} args)",
            f.func.name,
            args.positional.len()
        ))
        .with_span(f.func.span))
    }

    /// Token the runtime uses to cancel this VM (and its forks).
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }
}
