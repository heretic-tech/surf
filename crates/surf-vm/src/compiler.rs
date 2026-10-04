//! AST → bytecode.
//!
//! One pass over the AST per function, with a cheap pre-scan of each
//! function body that collects every name it assigns so that locals are
//! function-scoped regardless of where the first assignment appears
//! (`docs/language.md` § 3.1). Hoisted items (`fn`, `task`, `actor`,
//! `supervisor`, `on …:`, `browser:`) are compiled first, into the main
//! chunk's prologue, as nested closures of `<main>` — so they can see
//! top-level variables — and handed to the host with `Op::Declare` before
//! the first statement runs.

use crate::bytecode::{Chunk, CompiledProgram, DeclKind, Function, Op, UpvalueDesc};
use crate::value::Value;
use std::collections::HashMap;
use std::rc::Rc;
use surf_syntax::ast::{
    Arg, BinaryOp, Block, Expr, ExprKind, Item, LambdaBody, MapKey, Param, Program, Property, Stmt,
    StmtKind, StrSegment, UnaryOp,
};
use surf_syntax::{Diagnostic, Diagnostics, Span};

/// A compile-time error (undefined break target, assignment to a non-place,
/// property line after a statement, …).
#[derive(Debug, Clone, PartialEq)]
pub struct CompileError {
    /// Message.
    pub message: String,
    /// Where.
    pub span: Span,
}

impl CompileError {
    fn new(message: impl Into<String>, span: Span) -> Self {
        Self {
            message: message.into(),
            span,
        }
    }

    /// Convert to renderable diagnostics.
    pub fn into_diagnostics(self, name: &str) -> Diagnostics {
        Diagnostics::single(name, Diagnostic::error(self.message, self.span))
    }
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CompileError {}

/// Compile a parsed program. Without the source text, `line` in caught
/// error values is `nil`; prefer [`compile_with_source`].
pub fn compile(name: &str, program: &Program) -> Result<CompiledProgram, CompileError> {
    Compiler::new(Rc::from(Vec::new())).program(name, program)
}

/// Compile a parsed program, keeping a line table so runtime errors can
/// report their line (`e.line` in `catch`).
pub fn compile_with_source(
    name: &str,
    source: &str,
    program: &Program,
) -> Result<CompiledProgram, CompileError> {
    let mut starts = vec![0u32];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i as u32 + 1);
        }
    }
    Compiler::new(Rc::from(starts)).program(name, program)
}

type Res<T = ()> = Result<T, CompileError>;

/// Where a name lives.
#[derive(Debug, Clone, Copy)]
enum Var {
    Local(u32),
    Upvalue(u32),
    Global,
}

/// Compile-time block context used by `break` / `continue` / `return` to
/// know what to unwind.
enum Ctx {
    Loop {
        is_for: bool,
        start: usize,
        breaks: Vec<usize>,
    },
    Try,
}

#[derive(Hash, PartialEq, Eq)]
enum ConstKey {
    Int(i64),
    Float(u64),
    Str(Rc<str>),
}

struct FnState {
    name: Rc<str>,
    params: Vec<Rc<str>>,
    has_default: Vec<bool>,
    locals: Vec<Rc<str>>,
    upvalues: Vec<(UpvalueDesc, Rc<str>)>,
    chunk: Chunk,
    ctx: Vec<Ctx>,
    consts: HashMap<ConstKey, u32>,
    span: Span,
    /// Config-block mode: unresolved bare identifiers are symbols (strings).
    symbols: bool,
}

struct Compiler {
    fns: Vec<FnState>,
    line_starts: Rc<[u32]>,
}

impl Compiler {
    fn new(line_starts: Rc<[u32]>) -> Self {
        Self {
            fns: Vec::new(),
            line_starts,
        }
    }

    // ----- program ---------------------------------------------------------

    fn program(mut self, name: &str, program: &Program) -> Res<CompiledProgram> {
        self.begin_function("<main>", &[], program.span)?;
        let stmts: Vec<&Stmt> = program
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Stmt(s) => Some(s),
                _ => None,
            })
            .collect();
        let mut assigned = Vec::new();
        for s in &stmts {
            collect_assigned(s, &mut assigned);
        }
        self.declare_assigned(&assigned);

        // Prologue: hoisted declarations in source order.
        for item in &program.items {
            match item {
                Item::Config(c) => {
                    let name = self.name_const(&c.kind.name);
                    let alias = match &c.alias {
                        Some(a) => self.name_const(&a.name),
                        None => u32::MAX,
                    };
                    self.cur().symbols = true;
                    let r = self.config_props(&c.props);
                    self.cur().symbols = false;
                    r?;
                    self.emit(
                        Op::Declare {
                            kind: DeclKind::Config,
                            name,
                            alias,
                            props: c.props.len() as u32,
                        },
                        c.span,
                    );
                }
                Item::Fn(f) => {
                    let idx = self.function_block(&f.name.name, &f.params, &f.body, f.span)?;
                    self.emit(Op::Closure(idx), f.span);
                    let name = self.name_const(&f.name.name);
                    self.emit(
                        Op::Declare {
                            kind: DeclKind::Fn,
                            name,
                            alias: u32::MAX,
                            props: 0,
                        },
                        f.span,
                    );
                }
                Item::Task(t) => {
                    self.props(&t.props)?;
                    let idx = self.function_block(&t.name.name, &t.params, &t.body, t.span)?;
                    self.emit(Op::Closure(idx), t.span);
                    let name = self.name_const(&t.name.name);
                    self.emit(
                        Op::Declare {
                            kind: DeclKind::Task,
                            name,
                            alias: u32::MAX,
                            props: t.props.len() as u32,
                        },
                        t.span,
                    );
                }
                Item::Actor(a) => {
                    self.props(&a.props)?;
                    let idx = self.function_block(&a.name.name, &a.params, &a.body, a.span)?;
                    self.emit(Op::Closure(idx), a.span);
                    let name = self.name_const(&a.name.name);
                    self.emit(
                        Op::Declare {
                            kind: DeclKind::Actor,
                            name,
                            alias: u32::MAX,
                            props: a.props.len() as u32,
                        },
                        a.span,
                    );
                }
                Item::Supervisor(s) => {
                    self.props(&s.props)?;
                    let idx = self.function_block(&s.name.name, &[], &s.body, s.span)?;
                    self.emit(Op::Closure(idx), s.span);
                    let name = self.name_const(&s.name.name);
                    self.emit(
                        Op::Declare {
                            kind: DeclKind::Supervisor,
                            name,
                            alias: u32::MAX,
                            props: s.props.len() as u32,
                        },
                        s.span,
                    );
                }
                Item::Handler(h) => self.handler(h)?,
                Item::Stmt(_) => {}
            }
        }
        for s in stmts {
            self.stmt(s)?;
        }
        self.emit(Op::Nil, Span::at(program.span.end));
        self.emit(Op::Return, Span::at(program.span.end));
        let state = self.fns.pop().expect("main state");
        let main = self.finish(state);
        Ok(CompiledProgram {
            source_name: name.to_string(),
            main,
        })
    }

    fn handler(&mut self, h: &surf_syntax::ast::HandlerDecl) -> Res {
        for a in &h.args {
            self.expr(a)?;
        }
        let event_param = Param {
            name: surf_syntax::ast::Ident {
                name: "event".into(),
                span: h.event.span,
            },
            default: None,
            span: h.event.span,
        };
        let fname = format!("on {}", h.event.name);
        let idx =
            self.function_block(&fname, std::slice::from_ref(&event_param), &h.body, h.span)?;
        self.emit(Op::Closure(idx), h.span);
        let name = self.name_const(&h.event.name);
        self.emit(
            Op::Declare {
                kind: DeclKind::Handler,
                name,
                alias: u32::MAX,
                props: h.args.len() as u32,
            },
            h.span,
        );
        Ok(())
    }

    /// Config props: `(name, value)` pairs, eagerly evaluated, then `Map`.
    fn config_props(&mut self, props: &[Property]) -> Res {
        for p in props {
            let k = self.name_const(&p.key.name);
            self.emit(Op::Const(k), p.key.span);
            self.expr(&p.value)?;
        }
        self.emit(Op::Map(props.len() as u32), Span::default());
        Ok(())
    }

    /// Task / actor / supervisor props: `(name, value, lazy)` triples.
    /// Literals (and bare symbols such as `strategy: one_for_one`) are
    /// constants; anything else becomes a zero-arg thunk.
    fn props(&mut self, props: &[Property]) -> Res {
        for p in props {
            let k = self.name_const(&p.key.name);
            self.emit(Op::Const(k), p.key.span);
            let symbol = match &p.value.kind {
                ExprKind::Ident(id) => matches!(self.resolve(&id.name), Var::Global),
                _ => false,
            };
            if symbol {
                let ExprKind::Ident(id) = &p.value.kind else {
                    unreachable!()
                };
                let c = self.name_const(&id.name);
                self.emit(Op::Const(c), p.value.span);
                self.emit(Op::Bool(false), p.span);
            } else if is_literal(&p.value) {
                self.expr(&p.value)?;
                self.emit(Op::Bool(false), p.span);
            } else {
                let name = format!("<prop {}>", p.key.name);
                let idx = self.function_expr(&name, &[], &p.value, p.span)?;
                self.emit(Op::Closure(idx), p.span);
                self.emit(Op::Bool(true), p.span);
            }
        }
        Ok(())
    }

    // ----- functions -------------------------------------------------------

    fn cur(&mut self) -> &mut FnState {
        self.fns.last_mut().expect("function state")
    }

    fn begin_function(&mut self, name: &str, params: &[Param], span: Span) -> Res {
        let mut state = FnState {
            name: Rc::from(name),
            params: Vec::new(),
            has_default: Vec::new(),
            locals: Vec::new(),
            upvalues: Vec::new(),
            chunk: Chunk::default(),
            ctx: Vec::new(),
            consts: HashMap::new(),
            span,
            symbols: false,
        };
        for p in params {
            let n: Rc<str> = Rc::from(p.name.name.as_str());
            if state.locals.contains(&n) {
                return Err(CompileError::new(
                    format!("duplicate parameter `{}`", p.name.name),
                    p.name.span,
                ));
            }
            state.params.push(n.clone());
            state.has_default.push(p.default.is_some());
            state.locals.push(n);
        }
        self.fns.push(state);
        for (slot, p) in params.iter().enumerate() {
            if let Some(d) = &p.default {
                let at = self.emit(
                    Op::ParamDefault {
                        slot: slot as u32,
                        skip: 0,
                    },
                    p.span,
                );
                self.expr(d)?;
                self.emit(Op::SetLocal(slot as u32), p.span);
                let here = self.here();
                self.patch(at, here);
            }
        }
        Ok(())
    }

    /// Pre-declare every name the body assigns (unless it already resolves
    /// to a variable of an enclosing function — then assignment updates the
    /// captured variable instead of shadowing it).
    fn declare_assigned(&mut self, names: &[String]) {
        for n in names {
            let idx = self.fns.len() - 1;
            if self.fns[idx].locals.iter().any(|l| &**l == n.as_str()) {
                continue;
            }
            if idx > 0 && !matches!(self.resolve_at(idx - 1, n), Var::Global) {
                continue;
            }
            self.fns[idx].locals.push(Rc::from(n.as_str()));
        }
    }

    fn end_function(&mut self, span: Span) -> u32 {
        self.emit(Op::Nil, Span::at(span.end));
        self.emit(Op::Return, Span::at(span.end));
        let state = self.fns.pop().expect("function state");
        let f = self.finish(state);
        let parent = self.cur();
        parent.chunk.functions.push(f);
        (parent.chunk.functions.len() - 1) as u32
    }

    fn finish(&self, state: FnState) -> Rc<Function> {
        Rc::new(Function {
            name: state.name,
            params: state.params,
            has_default: state.has_default,
            locals: state.locals.len() as u32,
            upvalues: state.upvalues.iter().map(|(d, _)| *d).collect(),
            chunk: state.chunk,
            span: state.span,
            line_starts: self.line_starts.clone(),
        })
    }

    fn function_block(
        &mut self,
        name: &str,
        params: &[Param],
        body: &Block,
        span: Span,
    ) -> Res<u32> {
        self.begin_function(name, params, span)?;
        let mut assigned = Vec::new();
        for s in &body.stmts {
            collect_assigned(s, &mut assigned);
        }
        self.declare_assigned(&assigned);
        self.block(body)?;
        Ok(self.end_function(span))
    }

    fn function_expr(&mut self, name: &str, params: &[Param], body: &Expr, span: Span) -> Res<u32> {
        self.begin_function(name, params, span)?;
        self.expr(body)?;
        self.emit(Op::Return, body.span);
        Ok(self.end_function(span))
    }

    // ----- names -----------------------------------------------------------

    fn resolve(&mut self, name: &str) -> Var {
        let idx = self.fns.len() - 1;
        self.resolve_at(idx, name)
    }

    fn resolve_at(&mut self, idx: usize, name: &str) -> Var {
        if let Some(i) = self.fns[idx].locals.iter().position(|l| &**l == name) {
            return Var::Local(i as u32);
        }
        if let Some(i) = self.fns[idx]
            .upvalues
            .iter()
            .position(|(_, n)| &**n == name)
        {
            return Var::Upvalue(i as u32);
        }
        if idx == 0 {
            return Var::Global;
        }
        let desc = match self.resolve_at(idx - 1, name) {
            Var::Local(i) => UpvalueDesc {
                is_local: true,
                index: i,
            },
            Var::Upvalue(i) => UpvalueDesc {
                is_local: false,
                index: i,
            },
            Var::Global => return Var::Global,
        };
        let st = &mut self.fns[idx];
        st.upvalues.push((desc, Rc::from(name)));
        Var::Upvalue((st.upvalues.len() - 1) as u32)
    }

    fn declare_local(&mut self, name: &str) -> u32 {
        let st = self.cur();
        st.locals.push(Rc::from(name));
        (st.locals.len() - 1) as u32
    }

    // ----- emission --------------------------------------------------------

    fn emit(&mut self, op: Op, span: Span) -> usize {
        let chunk = &mut self.cur().chunk;
        chunk.code.push(op);
        chunk.spans.push(span);
        chunk.code.len() - 1
    }

    fn here(&mut self) -> usize {
        self.cur().chunk.code.len()
    }

    fn patch(&mut self, at: usize, target: usize) {
        let t = target as u32;
        let op = &mut self.cur().chunk.code[at];
        match op {
            Op::Jump(x)
            | Op::Loop(x)
            | Op::JumpIfFalse(x)
            | Op::JumpIfFalseKeep(x)
            | Op::JumpIfTrueKeep(x)
            | Op::IterNext(x)
            | Op::TryBegin(x) => *x = t,
            Op::ParamDefault { skip, .. } => *skip = t,
            other => panic!("patch of non-jump op {other:?}"),
        }
    }

    fn constant(&mut self, v: Value) -> u32 {
        let key = match &v {
            Value::Int(i) => Some(ConstKey::Int(*i)),
            Value::Float(f) => Some(ConstKey::Float(f.to_bits())),
            Value::Str(s) => Some(ConstKey::Str(s.clone())),
            _ => None,
        };
        let st = self.cur();
        if let Some(k) = &key {
            if let Some(&i) = st.consts.get(k) {
                return i;
            }
        }
        st.chunk.constants.push(v);
        let i = (st.chunk.constants.len() - 1) as u32;
        if let Some(k) = key {
            st.consts.insert(k, i);
        }
        i
    }

    fn name_const(&mut self, name: &str) -> u32 {
        self.constant(Value::str(name))
    }

    // ----- statements ------------------------------------------------------

    fn block(&mut self, b: &Block) -> Res {
        for s in &b.stmts {
            self.stmt(s)?;
        }
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt) -> Res {
        let span = s.span;
        match &s.kind {
            StmtKind::Expr(e) => {
                self.expr(e)?;
                self.emit(Op::Pop, span);
            }
            StmtKind::Assign { target, value } => self.assign(target, value, span)?,
            StmtKind::If {
                branches,
                else_block,
            } => {
                let mut ends = Vec::new();
                for (cond, block) in branches {
                    self.expr(cond)?;
                    let next = self.emit(Op::JumpIfFalse(0), cond.span);
                    self.block(block)?;
                    ends.push(self.emit(Op::Jump(0), span));
                    let here = self.here();
                    self.patch(next, here);
                }
                if let Some(b) = else_block {
                    self.block(b)?;
                }
                let here = self.here();
                for e in ends {
                    self.patch(e, here);
                }
            }
            StmtKind::For { var, iter, body } => {
                self.expr(iter)?;
                self.emit(Op::IterStart, iter.span);
                let start = self.emit(Op::IterNext(0), span);
                self.set_var(&var.name, var.span);
                self.cur().ctx.push(Ctx::Loop {
                    is_for: true,
                    start,
                    breaks: Vec::new(),
                });
                self.block(body)?;
                self.emit(Op::Loop(start as u32), span);
                let exit = self.here();
                self.patch(start, exit);
                self.end_loop(exit);
            }
            StmtKind::ParallelFor {
                var,
                iter,
                opts,
                body,
            } => {
                self.expr(iter)?;
                let p = Param {
                    name: var.clone(),
                    default: None,
                    span: var.span,
                };
                let idx =
                    self.function_block("<parallel for>", std::slice::from_ref(&p), body, span)?;
                self.emit(Op::Closure(idx), span);
                for o in opts {
                    let k = self.name_const(&o.key.name);
                    self.emit(Op::Const(k), o.key.span);
                    self.expr(&o.value)?;
                }
                self.emit(
                    Op::ParallelFor {
                        kwargs: opts.len() as u32,
                    },
                    span,
                );
                self.emit(Op::Pop, span);
            }
            StmtKind::While { cond, body } => {
                let start = self.here();
                self.expr(cond)?;
                let exit_jump = self.emit(Op::JumpIfFalse(0), cond.span);
                self.cur().ctx.push(Ctx::Loop {
                    is_for: false,
                    start,
                    breaks: Vec::new(),
                });
                self.block(body)?;
                self.emit(Op::Loop(start as u32), span);
                let exit = self.here();
                self.patch(exit_jump, exit);
                self.end_loop(exit);
            }
            StmtKind::Loop { body } => {
                let start = self.here();
                self.cur().ctx.push(Ctx::Loop {
                    is_for: false,
                    start,
                    breaks: Vec::new(),
                });
                self.block(body)?;
                self.emit(Op::Loop(start as u32), span);
                let exit = self.here();
                self.end_loop(exit);
            }
            StmtKind::Break => {
                let (is_for, _) = self.innermost_loop("break", span)?;
                self.unwind_tries_to_loop(span);
                if is_for {
                    self.emit(Op::IterEnd, span);
                }
                let j = self.emit(Op::Jump(0), span);
                for c in self.cur().ctx.iter_mut().rev() {
                    if let Ctx::Loop { breaks, .. } = c {
                        breaks.push(j);
                        break;
                    }
                }
            }
            StmtKind::Continue => {
                let (_, start) = self.innermost_loop("continue", span)?;
                self.unwind_tries_to_loop(span);
                self.emit(Op::Loop(start as u32), span);
            }
            StmtKind::Return(e) => {
                match e {
                    Some(e) => self.expr(e)?,
                    None => {
                        self.emit(Op::Nil, span);
                    }
                }
                let tries = self
                    .cur()
                    .ctx
                    .iter()
                    .filter(|c| matches!(c, Ctx::Try))
                    .count();
                for _ in 0..tries {
                    self.emit(Op::TryEnd, span);
                }
                self.emit(Op::Return, span);
            }
            StmtKind::Try {
                body,
                catch_var,
                catch_block,
            } => {
                let begin = self.emit(Op::TryBegin(0), span);
                self.cur().ctx.push(Ctx::Try);
                let r = self.block(body);
                self.cur().ctx.pop();
                r?;
                self.emit(Op::TryEnd, span);
                let skip = self.emit(Op::Jump(0), span);
                let handler = self.here();
                self.patch(begin, handler);
                match catch_var {
                    Some(v) => self.set_var(&v.name, v.span),
                    None => {
                        self.emit(Op::Pop, span);
                    }
                }
                self.block(catch_block)?;
                let end = self.here();
                self.patch(skip, end);
            }
            StmtKind::Emit(e) => {
                self.expr(e)?;
                self.emit(Op::Emit, span);
            }
            StmtKind::Exit(e) => {
                match e {
                    Some(e) => self.expr(e)?,
                    None => {
                        self.emit(Op::Nil, span);
                    }
                }
                self.emit(Op::Exit, span);
            }
            StmtKind::Fn(f) => {
                let idx = self.function_block(&f.name.name, &f.params, &f.body, f.span)?;
                self.emit(Op::Closure(idx), f.span);
                self.set_var(&f.name.name, f.name.span);
            }
            StmtKind::Handler(h) => self.handler(h)?,
        }
        Ok(())
    }

    fn innermost_loop(&mut self, what: &str, span: Span) -> Res<(bool, usize)> {
        for c in self.cur().ctx.iter().rev() {
            if let Ctx::Loop { is_for, start, .. } = c {
                return Ok((*is_for, *start));
            }
        }
        Err(CompileError::new(format!("`{what}` outside a loop"), span))
    }

    /// Emit the `TryEnd`s needed to leave every `try` between the current
    /// point and the innermost loop (for `break` / `continue`).
    fn unwind_tries_to_loop(&mut self, span: Span) {
        let mut n = 0;
        for c in self.cur().ctx.iter().rev() {
            match c {
                Ctx::Try => n += 1,
                Ctx::Loop { .. } => break,
            }
        }
        for _ in 0..n {
            self.emit(Op::TryEnd, span);
        }
    }

    fn end_loop(&mut self, exit: usize) {
        if let Some(Ctx::Loop { breaks, .. }) = self.cur().ctx.pop() {
            for b in breaks {
                self.patch(b, exit);
            }
        }
    }

    fn assign(&mut self, target: &Expr, value: &Expr, span: Span) -> Res {
        match &target.kind {
            ExprKind::Ident(id) => {
                self.expr(value)?;
                self.set_var(&id.name, id.span);
            }
            ExprKind::Field { receiver, name } => {
                self.expr(receiver)?;
                self.expr(value)?;
                let n = self.name_const(&name.name);
                self.emit(Op::SetField(n), span);
            }
            ExprKind::Index { receiver, index } => {
                self.expr(receiver)?;
                self.expr(index)?;
                self.expr(value)?;
                self.emit(Op::SetIndex, span);
            }
            _ => {
                return Err(CompileError::new(
                    "cannot assign to this expression (expected a name, `x.field` or `x[index]`)",
                    target.span,
                ))
            }
        }
        Ok(())
    }

    /// Pop the top of stack into variable `name` (creating a local if the
    /// name is unknown).
    fn set_var(&mut self, name: &str, span: Span) {
        match self.resolve(name) {
            Var::Local(i) => {
                self.emit(Op::SetLocal(i), span);
            }
            Var::Upvalue(i) => {
                self.emit(Op::SetUpvalue(i), span);
            }
            Var::Global => {
                let i = self.declare_local(name);
                self.emit(Op::SetLocal(i), span);
            }
        }
    }

    // ----- expressions -----------------------------------------------------

    fn expr(&mut self, e: &Expr) -> Res {
        let span = e.span;
        match &e.kind {
            ExprKind::Nil => {
                self.emit(Op::Nil, span);
            }
            ExprKind::Bool(b) => {
                self.emit(Op::Bool(*b), span);
            }
            ExprKind::Int(i) => {
                let c = self.constant(Value::Int(*i));
                self.emit(Op::Const(c), span);
            }
            ExprKind::Float(f) => {
                let c = self.constant(Value::Float(*f));
                self.emit(Op::Const(c), span);
            }
            ExprKind::Duration(d) => {
                let c = self.constant(Value::Duration(*d));
                self.emit(Op::Const(c), span);
            }
            ExprKind::Str(segments) => self.string(segments, span)?,
            ExprKind::Ident(id) => match self.resolve(&id.name) {
                Var::Local(i) => {
                    self.emit(Op::GetLocal(i), span);
                }
                Var::Upvalue(i) => {
                    self.emit(Op::GetUpvalue(i), span);
                }
                Var::Global => {
                    let c = self.name_const(&id.name);
                    if self.cur().symbols {
                        self.emit(Op::Const(c), span);
                    } else {
                        self.emit(Op::GetGlobal(c), span);
                    }
                }
            },
            ExprKind::List(items) => {
                for i in items {
                    self.expr(i)?;
                }
                self.emit(Op::List(items.len() as u32), span);
            }
            ExprKind::Map(entries) => {
                for (k, v) in entries {
                    match k {
                        MapKey::Ident(id) => {
                            let c = self.name_const(&id.name);
                            self.emit(Op::Const(c), id.span);
                        }
                        MapKey::Str(segments, s) => self.string(segments, *s)?,
                        MapKey::Expr(e) => self.expr(e)?,
                    }
                    self.expr(v)?;
                }
                self.emit(Op::Map(entries.len() as u32), span);
            }
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                self.expr(start)?;
                self.expr(end)?;
                self.emit(Op::Range(*inclusive), span);
            }
            ExprKind::Unary { op, expr } => {
                if let (UnaryOp::Neg, ExprKind::Int(i)) = (op, &expr.kind) {
                    let c = self.constant(Value::Int(i.wrapping_neg()));
                    self.emit(Op::Const(c), span);
                } else if let (UnaryOp::Neg, ExprKind::Float(f)) = (op, &expr.kind) {
                    let c = self.constant(Value::Float(-f));
                    self.emit(Op::Const(c), span);
                } else {
                    self.expr(expr)?;
                    self.emit(Op::Unary(*op), span);
                }
            }
            ExprKind::Binary { op, lhs, rhs } => match op {
                BinaryOp::And => {
                    self.expr(lhs)?;
                    let j = self.emit(Op::JumpIfFalseKeep(0), span);
                    self.emit(Op::Pop, span);
                    self.expr(rhs)?;
                    let here = self.here();
                    self.patch(j, here);
                }
                BinaryOp::Or => {
                    self.expr(lhs)?;
                    let j = self.emit(Op::JumpIfTrueKeep(0), span);
                    self.emit(Op::Pop, span);
                    self.expr(rhs)?;
                    let here = self.here();
                    self.patch(j, here);
                }
                _ => {
                    self.expr(lhs)?;
                    self.expr(rhs)?;
                    self.emit(Op::Binary(*op), span);
                }
            },
            ExprKind::Call { callee, args } => {
                if let ExprKind::Ident(id) = &callee.kind {
                    if let Var::Global = self.resolve(&id.name) {
                        let (positional, kwargs) = self.args(args)?;
                        let name = self.name_const(&id.name);
                        self.emit(
                            Op::CallGlobal {
                                name,
                                positional,
                                kwargs,
                            },
                            span,
                        );
                        return Ok(());
                    }
                }
                self.expr(callee)?;
                let (positional, kwargs) = self.args(args)?;
                self.emit(Op::Call { positional, kwargs }, span);
            }
            ExprKind::Method {
                receiver,
                name,
                args,
            } => {
                self.expr(receiver)?;
                let (positional, kwargs) = self.args(args)?;
                let n = self.name_const(&name.name);
                self.emit(
                    Op::CallMethod {
                        name: n,
                        positional,
                        kwargs,
                    },
                    span,
                );
            }
            ExprKind::Field { receiver, name } => {
                self.expr(receiver)?;
                let n = self.name_const(&name.name);
                self.emit(Op::GetField(n), span);
            }
            ExprKind::Index { receiver, index } => {
                self.expr(receiver)?;
                self.expr(index)?;
                self.emit(Op::GetIndex, span);
            }
            ExprKind::Lambda { params, body } => {
                let idx = match body {
                    LambdaBody::Expr(e) => self.function_expr("<lambda>", params, e, span)?,
                    LambdaBody::Block(b) => self.function_block("<lambda>", params, b, span)?,
                };
                self.emit(Op::Closure(idx), span);
            }
            ExprKind::Spawn(inner) => match &inner.kind {
                ExprKind::Call { callee, args } => {
                    let name = match &callee.kind {
                        ExprKind::Ident(id) if matches!(self.resolve(&id.name), Var::Global) => {
                            id.name.clone()
                        }
                        _ => {
                            return Err(CompileError::new(
                                "`spawn` needs a named fn, task or actor (`spawn worker(1)`) or a method call",
                                callee.span,
                            ))
                        }
                    };
                    let (positional, kwargs) = self.args(args)?;
                    let n = self.name_const(&name);
                    self.emit(
                        Op::Spawn {
                            name: n,
                            positional,
                            kwargs,
                        },
                        span,
                    );
                }
                ExprKind::Method {
                    receiver,
                    name,
                    args,
                } => {
                    self.expr(receiver)?;
                    let (positional, kwargs) = self.args(args)?;
                    let n = self.name_const(&name.name);
                    self.emit(
                        Op::SpawnMethod {
                            name: n,
                            positional,
                            kwargs,
                        },
                        span,
                    );
                }
                _ => {
                    return Err(CompileError::new(
                        "`spawn` must be followed by a call",
                        inner.span,
                    ))
                }
            },
        }
        Ok(())
    }

    fn string(&mut self, segments: &[StrSegment], span: Span) -> Res {
        if segments.is_empty() {
            let c = self.constant(Value::str(""));
            self.emit(Op::Const(c), span);
            return Ok(());
        }
        if let [StrSegment::Lit(s)] = segments {
            let c = self.constant(Value::str(s));
            self.emit(Op::Const(c), span);
            return Ok(());
        }
        for seg in segments {
            match seg {
                StrSegment::Lit(s) => {
                    let c = self.constant(Value::str(s));
                    self.emit(Op::Const(c), span);
                }
                StrSegment::Expr(e) => self.expr(e)?,
            }
        }
        self.emit(Op::Interp(segments.len() as u32), span);
        Ok(())
    }

    /// Push call arguments: positionals, then `(name, value)` pairs.
    fn args(&mut self, args: &[Arg]) -> Res<(u32, u32)> {
        let mut positional = 0;
        let mut kwargs = 0;
        for a in args {
            match &a.name {
                None => {
                    if kwargs > 0 {
                        return Err(CompileError::new(
                            "positional argument after keyword argument",
                            a.span,
                        ));
                    }
                    self.expr(&a.value)?;
                    positional += 1;
                }
                Some(n) => {
                    let c = self.name_const(&n.name);
                    self.emit(Op::Const(c), n.span);
                    self.expr(&a.value)?;
                    kwargs += 1;
                }
            }
        }
        Ok((positional, kwargs))
    }
}

/// Names assigned by a statement (not descending into nested functions,
/// lambdas, handlers or `parallel for` bodies, which are their own scope).
fn collect_assigned(s: &Stmt, out: &mut Vec<String>) {
    let mut add = |name: &str| {
        if !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    };
    match &s.kind {
        StmtKind::Assign { target, .. } => {
            if let ExprKind::Ident(id) = &target.kind {
                add(&id.name);
            }
        }
        StmtKind::If {
            branches,
            else_block,
        } => {
            for (_, b) in branches {
                collect_block(b, out);
            }
            if let Some(b) = else_block {
                collect_block(b, out);
            }
        }
        StmtKind::For { var, body, .. } => {
            add(&var.name);
            collect_block(body, out);
        }
        StmtKind::While { body, .. } | StmtKind::Loop { body } => collect_block(body, out),
        StmtKind::Try {
            body,
            catch_var,
            catch_block,
        } => {
            if let Some(v) = catch_var {
                add(&v.name);
            }
            collect_block(body, out);
            collect_block(catch_block, out);
        }
        StmtKind::Fn(f) => add(&f.name.name),
        StmtKind::ParallelFor { .. }
        | StmtKind::Handler(_)
        | StmtKind::Expr(_)
        | StmtKind::Break
        | StmtKind::Continue
        | StmtKind::Return(_)
        | StmtKind::Emit(_)
        | StmtKind::Exit(_) => {}
    }
}

fn collect_block(b: &Block, out: &mut Vec<String>) {
    for s in &b.stmts {
        collect_assigned(s, out);
    }
}

/// Whether a property value can be evaluated at compile time (→ `Prop::Const`).
fn is_literal(e: &Expr) -> bool {
    match &e.kind {
        ExprKind::Nil
        | ExprKind::Bool(_)
        | ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Duration(_) => true,
        ExprKind::Str(segs) => segs.iter().all(|s| matches!(s, StrSegment::Lit(_))),
        ExprKind::Unary {
            op: UnaryOp::Neg,
            expr,
        } => matches!(expr.kind, ExprKind::Int(_) | ExprKind::Float(_)),
        ExprKind::List(items) => items.iter().all(is_literal),
        ExprKind::Map(entries) => entries
            .iter()
            .all(|(k, v)| !matches!(k, MapKey::Expr(_)) && is_literal(v)),
        _ => false,
    }
}
