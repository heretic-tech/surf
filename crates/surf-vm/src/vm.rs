//! The interpreter.
//!
//! A classic stack machine. Script-to-script calls push a frame and keep
//! running in the same loop (no Rust recursion); every native call —
//! `Host::call_global`, `NativeObject::call_method`, stdlib — returns a
//! boxed future the loop awaits in place. Natives that need to call back
//! into script code (`xs.map(f)`, task bodies) use [`Vm::call`], which
//! runs a nested loop on the same stack.
//!
//! Captured variables are *boxed on capture*: a local slot is upgraded to a
//! shared cell the first time a closure captures it, so later assignments
//! on either side are visible to both (`docs/language.md` § 2.8, § 3.1).

use crate::bytecode::{CompiledProgram, DeclKind, Function, Op};
use crate::cancel::CancelToken;
use crate::error::RuntimeError;
use crate::host::{Declaration, Host, Prop};
use crate::stdlib::{self, Globals};
use crate::value::{Args, Closure, Range, Upvalue, Value};
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use surf_syntax::ast::{BinaryOp, UnaryOp};

/// One stack slot: a plain value, a cell shared with a closure, or a local
/// that nothing has assigned yet (reading it is the `used before
/// assignment` error).
#[derive(Clone)]
enum Slot {
    Val(Value),
    Boxed(Upvalue),
    Unset,
}

impl Slot {
    fn get(&self) -> Value {
        match self {
            Slot::Val(v) => v.clone(),
            Slot::Boxed(b) => b.borrow().clone(),
            Slot::Unset => Value::Nil,
        }
    }

    fn set(&mut self, v: Value) {
        match self {
            Slot::Val(x) => *x = v,
            Slot::Boxed(b) => *b.borrow_mut() = v,
            Slot::Unset => *self = Slot::Val(v),
        }
    }
}

/// A live `for` iterator.
enum Iter {
    List(Rc<RefCell<Vec<Value>>>, usize),
    Items(Vec<Value>, usize),
    Range(i64, i64),
}

impl Iter {
    fn next(&mut self) -> Option<Value> {
        match self {
            Iter::List(l, i) => {
                let v = l.borrow().get(*i).cloned();
                *i += 1;
                v
            }
            Iter::Items(items, i) => {
                let v = items.get(*i).cloned();
                *i += 1;
                v
            }
            Iter::Range(cur, end) => {
                if *cur < *end {
                    let v = *cur;
                    *cur += 1;
                    Some(Value::Int(v))
                } else {
                    None
                }
            }
        }
    }
}

struct Frame {
    closure: Rc<Closure>,
    pc: usize,
    base: usize,
    /// Bit `i` set → parameter `i` was not supplied (its default runs).
    missing: u64,
    iters: Vec<Iter>,
}

struct TryHandler {
    frame_depth: usize,
    pc: usize,
    stack_len: usize,
    iters_len: usize,
}

enum Step {
    Continue,
    Done(Value),
}

/// One interpreter instance: a value stack and call frames. The runtime
/// creates one `Vm` per concurrent task (spawned task, actor, handler
/// invocation, `parallel for` item); they all share the same `Host` and
/// `Globals`.
pub struct Vm {
    host: Rc<dyn Host>,
    globals: Rc<Globals>,
    cancel: CancelToken,
    stack: Vec<Slot>,
    frames: Vec<Frame>,
    handlers: Vec<TryHandler>,
}

impl Vm {
    /// Create a VM bound to `host` and `globals`.
    pub fn new(host: Rc<dyn Host>, globals: Rc<Globals>) -> Vm {
        Vm {
            host,
            globals,
            cancel: CancelToken::new(),
            stack: Vec::new(),
            frames: Vec::new(),
            handlers: Vec::new(),
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
            frames: Vec::new(),
            handlers: Vec::new(),
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

    /// Token the runtime uses to cancel this VM (and its forks).
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Run a compiled program to completion (prologue declarations, then
    /// statements). Returns when the main body finishes; the runtime decides
    /// whether to keep the process alive for handlers / tasks.
    pub async fn run(&mut self, program: &CompiledProgram) -> Result<(), RuntimeError> {
        let main = Rc::new(Closure::new(program.main.clone()));
        self.call(main, Args::default()).await.map(|_| ())
    }

    /// Call a closure with arguments and run it to completion.
    pub async fn call(&mut self, f: Rc<Closure>, args: Args) -> Result<Value, RuntimeError> {
        self.check_cancel()?;
        let depth = self.frames.len();
        let stack_len = self.stack.len();
        let handlers_len = self.handlers.len();
        let r = match self.push_frame(f, args) {
            Ok(()) => self.run_frames(depth).await,
            Err(e) => Err(e),
        };
        if r.is_err() {
            self.frames.truncate(depth);
            self.stack.truncate(stack_len);
            self.handlers.truncate(handlers_len);
        }
        r
    }

    /// Call any callable value: a script closure, or a native object's
    /// `__call__` method.
    pub fn call_value<'a>(
        &'a mut self,
        callee: Value,
        args: Args,
    ) -> futures::future::LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        Box::pin(async move {
            match callee {
                Value::Fn(f) => self.call(f, args).await,
                Value::Native(n) => n.call_method(self, "__call__", args).await,
                other => Err(RuntimeError::new(format!(
                    "cannot call a value of type {}",
                    other.type_name()
                ))),
            }
        })
    }

    fn check_cancel(&self) -> Result<(), RuntimeError> {
        if self.cancel.is_cancelled() {
            Err(RuntimeError::cancelled())
        } else {
            Ok(())
        }
    }

    // ----- stack helpers ---------------------------------------------------

    fn push(&mut self, v: Value) {
        self.stack.push(Slot::Val(v));
    }

    fn pop(&mut self) -> Value {
        match self.stack.pop() {
            Some(s) => s.get(),
            None => Value::Nil,
        }
    }

    fn pop_n(&mut self, n: usize) -> Vec<Value> {
        let at = self.stack.len().saturating_sub(n);
        self.stack.drain(at..).map(|s| s.get()).collect()
    }

    fn peek(&self) -> Value {
        self.stack.last().map(Slot::get).unwrap_or(Value::Nil)
    }

    /// Pop `positional` values and `kwargs` `(name, value)` pairs into
    /// [`Args`].
    fn pop_args(&mut self, positional: u32, kwargs: u32) -> Result<Args, RuntimeError> {
        let mut kw = IndexMap::new();
        let pairs = self.pop_n(kwargs as usize * 2);
        for pair in pairs.chunks(2) {
            let name = match &pair[0] {
                Value::Str(s) => s.clone(),
                other => {
                    return Err(RuntimeError::new(format!(
                        "keyword argument name must be a string, got {}",
                        other.type_name()
                    )))
                }
            };
            kw.insert(name, pair[1].clone());
        }
        let positional = self.pop_n(positional as usize);
        Ok(Args {
            positional,
            kwargs: kw,
        })
    }

    fn frame(&self) -> &Frame {
        self.frames.last().expect("active frame")
    }

    fn frame_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().expect("active frame")
    }

    fn func(&self) -> &Function {
        &self.frame().closure.func
    }

    fn constant(&self, i: u32) -> Value {
        self.func().chunk.constants[i as usize].clone()
    }

    fn const_name(&self, i: u32) -> Rc<str> {
        match &self.func().chunk.constants[i as usize] {
            Value::Str(s) => s.clone(),
            other => Rc::from(other.to_string()),
        }
    }

    fn local_slot(&mut self, i: u32) -> &mut Slot {
        let base = self.frame().base;
        &mut self.stack[base + i as usize]
    }

    fn box_local(&mut self, i: u32) -> Upvalue {
        let slot = self.local_slot(i);
        match slot {
            Slot::Boxed(b) => b.clone(),
            // A captured-before-assigned local starts as `nil` in the cell:
            // the closure may run before the assignment and the sentinel
            // does not travel into upvalues.
            Slot::Val(_) | Slot::Unset => {
                let b = Rc::new(RefCell::new(slot.get()));
                *slot = Slot::Boxed(b.clone());
                b
            }
        }
    }

    // ----- frames ----------------------------------------------------------

    /// Bind `args` to a new frame for `closure` (positional first, then
    /// keywords by parameter name; defaults are evaluated by the callee's
    /// `ParamDefault` prologue).
    fn push_frame(&mut self, closure: Rc<Closure>, args: Args) -> Result<(), RuntimeError> {
        let func = closure.func.clone();
        let nparams = func.params.len();
        if args.positional.len() > nparams {
            return Err(RuntimeError::new(format!(
                "{} takes {} argument{} but {} were given",
                describe(&func),
                nparams,
                if nparams == 1 { "" } else { "s" },
                args.positional.len()
            ))
            .with_span(func.span));
        }
        let base = self.stack.len();
        let given = args.positional.len();
        let mut missing: u64 = 0;
        for v in args.positional {
            self.push(v);
        }
        for i in given..(func.locals as usize) {
            if i < nparams {
                // A parameter: bound below by a keyword, a default or the
                // missing-argument error.
                self.push(Value::Nil);
                if i < 64 {
                    missing |= 1 << i;
                }
            } else {
                self.stack.push(Slot::Unset);
            }
        }
        for (k, v) in args.kwargs {
            let Some(i) = func.params.iter().position(|p| *p == k) else {
                self.stack.truncate(base);
                return Err(RuntimeError::new(format!(
                    "{} has no parameter `{k}`",
                    describe(&func)
                ))
                .with_span(func.span));
            };
            if i < given {
                self.stack.truncate(base);
                return Err(RuntimeError::new(format!(
                    "{}: argument `{k}` given twice",
                    describe(&func)
                ))
                .with_span(func.span));
            }
            self.stack[base + i] = Slot::Val(v);
            if i < 64 {
                missing &= !(1 << i);
            }
        }
        for i in given..nparams {
            if i < 64 && missing & (1 << i) != 0 && !func.has_default[i] {
                self.stack.truncate(base);
                return Err(RuntimeError::new(format!(
                    "{}: missing argument `{}`",
                    describe(&func),
                    func.params[i]
                ))
                .with_span(func.span));
            }
        }
        self.frames.push(Frame {
            closure,
            pc: 0,
            base,
            missing,
            iters: Vec::new(),
        });
        Ok(())
    }

    async fn run_frames(&mut self, entry_depth: usize) -> Result<Value, RuntimeError> {
        let handler_base = self.handlers.len();
        loop {
            let (op, span) = {
                let f = self.frame();
                let chunk = &f.closure.func.chunk;
                (chunk.code[f.pc], chunk.spans[f.pc])
            };
            self.frame_mut().pc += 1;
            match self.exec(op, entry_depth).await {
                Ok(Step::Continue) => {}
                Ok(Step::Done(v)) => return Ok(v),
                Err(e) => {
                    let e = e.or_span(span);
                    if e.is_catchable() && self.handlers.len() > handler_base {
                        let h = self.handlers.pop().expect("handler");
                        self.frames.truncate(h.frame_depth + 1);
                        self.stack.truncate(h.stack_len);
                        let f = self.frame_mut();
                        f.iters.truncate(h.iters_len);
                        f.pc = h.pc;
                        let line = e.span.and_then(|s| f.closure.func.line_of(s.start));
                        self.push(e.to_value(line));
                    } else {
                        self.handlers.truncate(handler_base);
                        return Err(e);
                    }
                }
            }
        }
    }

    async fn exec(&mut self, op: Op, entry_depth: usize) -> Result<Step, RuntimeError> {
        match op {
            Op::Const(i) => {
                let v = self.constant(i);
                self.push(v);
            }
            Op::Nil => self.push(Value::Nil),
            Op::Bool(b) => self.push(Value::Bool(b)),
            Op::Pop => {
                self.stack.pop();
            }
            Op::Dup => {
                let v = self.peek();
                self.push(v);
            }
            Op::Dup2 => {
                let n = self.stack.len();
                let a = self
                    .stack
                    .get(n.wrapping_sub(2))
                    .map_or(Value::Nil, Slot::get);
                let b = self.peek();
                self.push(a);
                self.push(b);
            }
            Op::GetLocal(i) => {
                if matches!(self.local_slot(i), Slot::Unset) {
                    let name = self
                        .func()
                        .chunk
                        .local_names
                        .get(i as usize)
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "?".into());
                    return Err(RuntimeError::new(format!(
                        "variable `{name}` used before assignment"
                    )));
                }
                let v = self.local_slot(i).get();
                self.push(v);
            }
            Op::SetLocal(i) => {
                let v = self.pop();
                self.local_slot(i).set(v);
            }
            Op::GetUpvalue(i) => {
                let v = self.frame().closure.captures[i as usize].borrow().clone();
                self.push(v);
            }
            Op::SetUpvalue(i) => {
                let v = self.pop();
                *self.frame().closure.captures[i as usize].borrow_mut() = v;
            }
            Op::GetGlobal(i) => {
                let name = self.const_name(i);
                let v = self.get_global(&name)?;
                self.push(v);
            }
            Op::SetGlobal(i) => {
                let name = self.const_name(i);
                let v = self.pop();
                self.host.set_global(&name, v)?;
            }
            Op::List(n) => {
                let items = self.pop_n(n as usize);
                self.push(Value::list(items));
            }
            Op::Map(n) => {
                let items = self.pop_n(n as usize * 2);
                let mut m = IndexMap::with_capacity(n as usize);
                for pair in items.chunks(2) {
                    let key = match &pair[0] {
                        Value::Str(s) => s.clone(),
                        Value::Int(i) => Rc::from(i.to_string()),
                        other => {
                            return Err(RuntimeError::new(format!(
                                "map keys must be strings, got {}",
                                other.type_name()
                            )))
                        }
                    };
                    m.insert(key, pair[1].clone());
                }
                self.push(Value::map(m));
            }
            Op::Interp(n) => {
                let parts = self.pop_n(n as usize);
                let mut s = String::new();
                for p in parts {
                    use std::fmt::Write;
                    let _ = write!(s, "{p}");
                }
                self.push(Value::str(s));
            }
            Op::Range(inclusive) => {
                let end = self.pop();
                let start = self.pop();
                let (Value::Int(a), Value::Int(b)) = (&start, &end) else {
                    return Err(RuntimeError::new(format!(
                        "range bounds must be ints, got {} and {}",
                        start.type_name(),
                        end.type_name()
                    )));
                };
                let end = if inclusive { b.saturating_add(1) } else { *b };
                self.push(Value::native(Range { start: *a, end }));
            }
            Op::Unary(op) => {
                let v = self.pop();
                self.push(unary(op, v)?);
            }
            Op::Binary(op) => {
                let rhs = self.pop();
                let lhs = self.pop();
                self.push(binary(op, lhs, rhs)?);
            }
            Op::Jump(t) => self.frame_mut().pc = t as usize,
            Op::Loop(t) => {
                self.check_cancel()?;
                self.frame_mut().pc = t as usize;
            }
            Op::JumpIfFalse(t) => {
                if !self.pop().is_truthy() {
                    self.frame_mut().pc = t as usize;
                }
            }
            Op::JumpIfFalseKeep(t) => {
                if !self.peek().is_truthy() {
                    self.frame_mut().pc = t as usize;
                }
            }
            Op::JumpIfTrueKeep(t) => {
                if self.peek().is_truthy() {
                    self.frame_mut().pc = t as usize;
                }
            }
            Op::Call { positional, kwargs } => {
                let args = self.pop_args(positional, kwargs)?;
                let callee = self.pop();
                self.invoke(callee, args).await?;
            }
            Op::CallGlobal {
                name,
                positional,
                kwargs,
            } => {
                let args = self.pop_args(positional, kwargs)?;
                let name = self.const_name(name);
                if let Some(v) = self.host.resolve_global(&name) {
                    self.invoke(v, args).await?;
                } else if let Some(f) = self.globals.get(&name) {
                    self.check_cancel()?;
                    let v = f(self, args).await?;
                    self.push(v);
                } else {
                    self.check_cancel()?;
                    let host = self.host.clone();
                    let v = host.call_global(self, &name, args).await?;
                    self.push(v);
                }
            }
            Op::CallMethod {
                name,
                positional,
                kwargs,
            } => {
                let args = self.pop_args(positional, kwargs)?;
                let recv = self.pop();
                let name = self.const_name(name);
                self.check_cancel()?;
                let v = match recv {
                    Value::Native(n) => n.call_method(self, &name, args).await?,
                    other => stdlib::call_method(self, other, &name, args).await?,
                };
                self.push(v);
            }
            Op::GetField(i) => {
                let name = self.const_name(i);
                let recv = self.pop();
                let v = match &recv {
                    Value::Map(m) => m.borrow().get(&*name).cloned().unwrap_or(Value::Nil),
                    Value::Native(n) => n.get_prop(&name).ok_or_else(|| {
                        RuntimeError::new(format!("{} has no property `{name}`", n.type_name()))
                    })?,
                    other => {
                        return Err(RuntimeError::new(format!(
                            "cannot read property `{name}` of {}",
                            other.type_name()
                        )))
                    }
                };
                self.push(v);
            }
            Op::SetField(i) => {
                let name = self.const_name(i);
                let v = self.pop();
                let recv = self.pop();
                match &recv {
                    Value::Map(m) => {
                        m.borrow_mut().insert(name, v);
                    }
                    other => {
                        return Err(RuntimeError::new(format!(
                            "cannot set property `{name}` on {}",
                            other.type_name()
                        )))
                    }
                }
            }
            Op::GetIndex => {
                let idx = self.pop();
                let recv = self.pop();
                self.push(get_index(&recv, &idx)?);
            }
            Op::SetIndex => {
                let v = self.pop();
                let idx = self.pop();
                let recv = self.pop();
                set_index(&recv, &idx, v)?;
            }
            Op::Closure(i) => {
                let func = self.func().chunk.functions[i as usize].clone();
                let mut captures = Vec::with_capacity(func.upvalues.len());
                for d in &func.upvalues {
                    if d.is_local {
                        captures.push(self.box_local(d.index));
                    } else {
                        captures.push(self.frame().closure.captures[d.index as usize].clone());
                    }
                }
                self.push(Value::Fn(Rc::new(Closure { func, captures })));
            }
            Op::ParamDefault { slot, skip } => {
                let f = self.frame_mut();
                if slot >= 64 || f.missing & (1 << slot) == 0 {
                    f.pc = skip as usize;
                }
            }
            Op::Return => {
                let v = self.pop();
                let frame = self.frames.pop().expect("frame");
                self.stack.truncate(frame.base);
                let depth = self.frames.len();
                self.handlers.retain(|h| h.frame_depth < depth);
                if depth == entry_depth {
                    return Ok(Step::Done(v));
                }
                self.push(v);
            }
            Op::IterStart => {
                let v = self.pop();
                let it = match &v {
                    Value::List(l) => Iter::List(l.clone(), 0),
                    Value::Str(s) => {
                        Iter::Items(s.chars().map(|c| Value::str(c.to_string())).collect(), 0)
                    }
                    Value::Map(m) => Iter::Items(
                        m.borrow().keys().map(|k| Value::Str(k.clone())).collect(),
                        0,
                    ),
                    Value::Native(n) => {
                        if let Some(r) = n.as_any().and_then(|a| a.downcast_ref::<Range>()) {
                            Iter::Range(r.start, r.end)
                        } else if let Some(items) = n.iter_items() {
                            Iter::Items(items, 0)
                        } else {
                            return Err(RuntimeError::new(format!(
                                "cannot iterate over {}",
                                n.type_name()
                            )));
                        }
                    }
                    other => {
                        return Err(RuntimeError::new(format!(
                            "cannot iterate over {}",
                            other.type_name()
                        )))
                    }
                };
                self.frame_mut().iters.push(it);
            }
            Op::IterNext(exit) => {
                let f = self.frame_mut();
                let next = f.iters.last_mut().and_then(Iter::next);
                match next {
                    Some(v) => self.push(v),
                    None => {
                        f.iters.pop();
                        f.pc = exit as usize;
                    }
                }
            }
            Op::IterEnd => {
                self.frame_mut().iters.pop();
            }
            Op::Emit => {
                let v = self.pop();
                self.host.emit(&v);
            }
            Op::Exit => {
                let code = match self.pop() {
                    Value::Nil => 0,
                    Value::Int(i) => i as i32,
                    Value::Bool(b) => {
                        if b {
                            0
                        } else {
                            1
                        }
                    }
                    other => {
                        return Err(RuntimeError::new(format!(
                            "exit code must be an int, got {}",
                            other.type_name()
                        )))
                    }
                };
                return Err(RuntimeError::exit(code));
            }
            Op::Spawn {
                name,
                positional,
                kwargs,
            } => {
                let args = self.pop_args(positional, kwargs)?;
                let name = self.const_name(name);
                self.check_cancel()?;
                let host = self.host.clone();
                let v = host.spawn(self, &name, args).await?;
                self.push(v);
            }
            Op::SpawnMethod {
                name,
                positional,
                kwargs,
            } => {
                let args = self.pop_args(positional, kwargs)?;
                let recv = self.pop();
                let name = self.const_name(name);
                self.check_cancel()?;
                let host = self.host.clone();
                let v = host.spawn_method(self, recv, &name, args).await?;
                self.push(v);
            }
            Op::SpawnValue { positional, kwargs } => {
                let args = self.pop_args(positional, kwargs)?;
                let callee = self.pop();
                self.check_cancel()?;
                let host = self.host.clone();
                let v = host.spawn_value(self, callee, args).await?;
                self.push(v);
            }
            Op::ParallelFor { kwargs } => {
                let opts = self.pop_args(0, kwargs)?;
                let body = match self.pop() {
                    Value::Fn(f) => f,
                    other => {
                        return Err(RuntimeError::new(format!(
                            "parallel for body must be a closure, got {}",
                            other.type_name()
                        )))
                    }
                };
                let items = self.pop();
                self.check_cancel()?;
                let host = self.host.clone();
                let v = host.parallel_for(self, items, body, opts).await?;
                self.push(v);
            }
            Op::TryBegin(pc) => {
                let frame_depth = self.frames.len() - 1;
                let stack_len = self.stack.len();
                let iters_len = self.frame().iters.len();
                self.handlers.push(TryHandler {
                    frame_depth,
                    pc: pc as usize,
                    stack_len,
                    iters_len,
                });
            }
            Op::TryEnd => {
                self.handlers.pop();
            }
            Op::Declare {
                kind,
                name,
                alias,
                props,
            } => {
                let decl = self.build_declaration(kind, name, alias, props)?;
                self.host.declare(decl);
            }
        }
        Ok(Step::Continue)
    }

    /// Call `callee` from inside the loop: closures push a frame, natives
    /// are awaited.
    async fn invoke(&mut self, callee: Value, args: Args) -> Result<(), RuntimeError> {
        match callee {
            Value::Fn(f) => self.push_frame(f, args),
            Value::Native(n) => {
                self.check_cancel()?;
                let v = n.call_method(self, "__call__", args).await?;
                self.push(v);
                Ok(())
            }
            other => Err(RuntimeError::new(format!(
                "cannot call a value of type {}",
                other.type_name()
            ))),
        }
    }

    fn get_global(&mut self, name: &str) -> Result<Value, RuntimeError> {
        if let Some(v) = self.host.resolve_global(name) {
            return Ok(v);
        }
        if let Some(v) = self.globals.get_value(name) {
            return Ok(v);
        }
        if let Some(f) = self.globals.get(name) {
            return Ok(Value::native(stdlib::BuiltinFn {
                name: Rc::from(name),
                f,
            }));
        }
        Err(RuntimeError::new(format!("undefined variable `{name}`")))
    }

    fn build_declaration(
        &mut self,
        kind: DeclKind,
        name: u32,
        alias: u32,
        props: u32,
    ) -> Result<Declaration, RuntimeError> {
        let name = self.const_name(name).to_string();
        let pop_closure = |vm: &mut Vm| -> Result<Rc<Closure>, RuntimeError> {
            match vm.pop() {
                Value::Fn(f) => Ok(f),
                other => Err(RuntimeError::new(format!(
                    "internal: expected a closure for declaration, got {}",
                    other.type_name()
                ))),
            }
        };
        let pop_props = |vm: &mut Vm, n: u32| -> Result<IndexMap<Rc<str>, Prop>, RuntimeError> {
            let items = vm.pop_n(n as usize * 3);
            let mut out = IndexMap::new();
            for t in items.chunks(3) {
                let key = match &t[0] {
                    Value::Str(s) => s.clone(),
                    other => Rc::from(other.to_string()),
                };
                let prop = match (&t[1], &t[2]) {
                    (Value::Fn(f), Value::Bool(true)) => Prop::Lazy(f.clone()),
                    (v, _) => Prop::Const(v.clone()),
                };
                out.insert(key, prop);
            }
            Ok(out)
        };
        Ok(match kind {
            DeclKind::Config => {
                let props = match self.pop() {
                    Value::Map(m) => m.borrow().clone(),
                    other => {
                        return Err(RuntimeError::new(format!(
                            "internal: expected a map for config, got {}",
                            other.type_name()
                        )))
                    }
                };
                let alias = if alias == u32::MAX {
                    None
                } else {
                    Some(self.const_name(alias).to_string())
                };
                Declaration::Config { name, alias, props }
            }
            DeclKind::Fn => Declaration::Fn {
                name,
                closure: pop_closure(self)?,
            },
            DeclKind::Task => {
                let body = pop_closure(self)?;
                let props = pop_props(self, props)?;
                Declaration::Task {
                    name,
                    params: body.func.params.clone(),
                    props,
                    body,
                }
            }
            DeclKind::Actor => {
                let body = pop_closure(self)?;
                let props = pop_props(self, props)?;
                Declaration::Actor {
                    name,
                    params: body.func.params.clone(),
                    props,
                    body,
                }
            }
            DeclKind::Supervisor => {
                let body = pop_closure(self)?;
                let props = pop_props(self, props)?;
                Declaration::Supervisor { name, props, body }
            }
            DeclKind::Handler => {
                let body = pop_closure(self)?;
                let args = self.pop_n(props as usize);
                Declaration::Handler {
                    event: name,
                    args,
                    body,
                }
            }
        })
    }
}

fn describe(f: &Function) -> String {
    if f.name.starts_with('<') {
        f.name.to_string()
    } else {
        format!("`{}`", f.name)
    }
}

// ----- operators -----------------------------------------------------------

fn unary(op: UnaryOp, v: Value) -> Result<Value, RuntimeError> {
    match op {
        UnaryOp::Not => Ok(Value::Bool(!v.is_truthy())),
        UnaryOp::Neg => match v {
            Value::Int(i) => i
                .checked_neg()
                .map(Value::Int)
                .ok_or_else(|| RuntimeError::new("integer overflow")),
            Value::Float(f) => Ok(Value::Float(-f)),
            other => Err(RuntimeError::new(format!(
                "cannot negate {}",
                other.type_name()
            ))),
        },
    }
}

fn type_error(op: &str, a: &Value, b: &Value) -> RuntimeError {
    RuntimeError::new(format!(
        "unsupported operand types for `{op}`: {} and {}",
        a.type_name(),
        b.type_name()
    ))
}

fn binary(op: BinaryOp, a: Value, b: Value) -> Result<Value, RuntimeError> {
    use BinaryOp::*;
    match op {
        Eq => Ok(Value::Bool(a.equals(&b))),
        NotEq => Ok(Value::Bool(!a.equals(&b))),
        Lt | LtEq | Gt | GtEq => {
            let ord = a
                .compare(&b)
                .ok_or_else(|| type_error(cmp_symbol(op), &a, &b))?;
            Ok(Value::Bool(match op {
                Lt => ord.is_lt(),
                LtEq => ord.is_le(),
                Gt => ord.is_gt(),
                _ => ord.is_ge(),
            }))
        }
        Add => match (&a, &b) {
            (Value::Int(x), Value::Int(y)) => x
                .checked_add(*y)
                .map(Value::Int)
                .ok_or_else(|| RuntimeError::new("integer overflow")),
            (Value::Str(x), Value::Str(y)) => Ok(Value::str(format!("{x}{y}"))),
            (Value::List(x), Value::List(y)) => {
                let mut v = x.borrow().clone();
                v.extend(y.borrow().iter().cloned());
                Ok(Value::list(v))
            }
            (Value::Duration(x), Value::Duration(y)) => Ok(Value::Duration(*x + *y)),
            _ => numeric(op, &a, &b, |x, y| x + y),
        },
        Sub => match (&a, &b) {
            (Value::Int(x), Value::Int(y)) => x
                .checked_sub(*y)
                .map(Value::Int)
                .ok_or_else(|| RuntimeError::new("integer overflow")),
            (Value::Duration(x), Value::Duration(y)) => Ok(Value::Duration(
                x.checked_sub(*y)
                    .ok_or_else(|| RuntimeError::new("duration underflow"))?,
            )),
            _ => numeric(op, &a, &b, |x, y| x - y),
        },
        Mul => match (&a, &b) {
            (Value::Int(x), Value::Int(y)) => x
                .checked_mul(*y)
                .map(Value::Int)
                .ok_or_else(|| RuntimeError::new("integer overflow")),
            (Value::Duration(d), n) | (n, Value::Duration(d)) if n.as_f64().is_some() => {
                let k = n.as_f64().unwrap_or(0.0);
                if k < 0.0 {
                    return Err(RuntimeError::new(
                        "cannot scale a duration by a negative number",
                    ));
                }
                Ok(Value::Duration(Duration::from_secs_f64(
                    d.as_secs_f64() * k,
                )))
            }
            _ => numeric(op, &a, &b, |x, y| x * y),
        },
        Div => match (&a, &b) {
            (Value::Int(x), Value::Int(y)) => {
                if *y == 0 {
                    Err(RuntimeError::new("division by zero"))
                } else {
                    x.checked_div(*y)
                        .map(Value::Int)
                        .ok_or_else(|| RuntimeError::new("integer overflow"))
                }
            }
            (Value::Duration(d), n) if n.as_f64().is_some() => {
                let k = n.as_f64().unwrap_or(0.0);
                if k == 0.0 {
                    return Err(RuntimeError::new("division by zero"));
                }
                Ok(Value::Duration(Duration::from_secs_f64(
                    d.as_secs_f64() / k,
                )))
            }
            _ => {
                if b.as_f64() == Some(0.0) && a.as_f64().is_some() {
                    return Err(RuntimeError::new("division by zero"));
                }
                numeric(op, &a, &b, |x, y| x / y)
            }
        },
        Rem => match (&a, &b) {
            (Value::Int(x), Value::Int(y)) => {
                if *y == 0 {
                    Err(RuntimeError::new("division by zero"))
                } else {
                    x.checked_rem(*y)
                        .map(Value::Int)
                        .ok_or_else(|| RuntimeError::new("integer overflow"))
                }
            }
            _ => {
                if b.as_f64() == Some(0.0) && a.as_f64().is_some() {
                    return Err(RuntimeError::new("division by zero"));
                }
                numeric(op, &a, &b, |x, y| x % y)
            }
        },
        And | Or => unreachable!("short-circuit ops are compiled to jumps"),
    }
}

fn cmp_symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Lt => "<",
        BinaryOp::LtEq => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::GtEq => ">=",
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Rem => "%",
        BinaryOp::Eq => "==",
        BinaryOp::NotEq => "!=",
        BinaryOp::And => "and",
        BinaryOp::Or => "or",
    }
}

fn numeric(
    op: BinaryOp,
    a: &Value,
    b: &Value,
    f: impl Fn(f64, f64) -> f64,
) -> Result<Value, RuntimeError> {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => Ok(Value::Float(f(x, y))),
        _ => Err(type_error(cmp_symbol(op), a, b)),
    }
}

fn index_of(len: usize, idx: &Value, what: &str) -> Result<usize, RuntimeError> {
    let Value::Int(i) = idx else {
        return Err(RuntimeError::new(format!(
            "{what} index must be an int, got {}",
            idx.type_name()
        )));
    };
    let n = len as i64;
    let j = if *i < 0 { n + *i } else { *i };
    if j < 0 || j >= n {
        return Err(RuntimeError::new(format!(
            "{what} index {i} out of range (len {len})"
        )));
    }
    Ok(j as usize)
}

fn get_index(recv: &Value, idx: &Value) -> Result<Value, RuntimeError> {
    match recv {
        Value::List(l) => {
            let l = l.borrow();
            let i = index_of(l.len(), idx, "list")?;
            Ok(l[i].clone())
        }
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            let i = index_of(chars.len(), idx, "string")?;
            Ok(Value::str(chars[i].to_string()))
        }
        Value::Map(m) => match idx {
            Value::Str(k) => Ok(m.borrow().get(&**k).cloned().unwrap_or(Value::Nil)),
            other => Err(RuntimeError::new(format!(
                "map key must be a string, got {}",
                other.type_name()
            ))),
        },
        Value::Native(n) => {
            if let Some(r) = n.as_any().and_then(|a| a.downcast_ref::<Range>()) {
                let i = index_of(r.len(), idx, "range")?;
                return Ok(Value::Int(r.start + i as i64));
            }
            Err(RuntimeError::new(format!("cannot index {}", n.type_name())))
        }
        other => Err(RuntimeError::new(format!(
            "cannot index {}",
            other.type_name()
        ))),
    }
}

fn set_index(recv: &Value, idx: &Value, v: Value) -> Result<(), RuntimeError> {
    match recv {
        Value::List(l) => {
            let mut l = l.borrow_mut();
            let i = index_of(l.len(), idx, "list")?;
            l[i] = v;
            Ok(())
        }
        Value::Map(m) => match idx {
            Value::Str(k) => {
                m.borrow_mut().insert(k.clone(), v);
                Ok(())
            }
            other => Err(RuntimeError::new(format!(
                "map key must be a string, got {}",
                other.type_name()
            ))),
        },
        other => Err(RuntimeError::new(format!(
            "cannot assign into {}",
            other.type_name()
        ))),
    }
}
