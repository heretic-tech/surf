//! Bytecode representation.

use crate::value::Value;
use std::rc::Rc;
use surf_syntax::Span;

/// Stack-VM opcodes. The final instruction set is defined by task 6; this
/// is the minimal skeleton other crates may reference.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// Push `constants[i]`.
    Const(u32),
    /// Push `nil`.
    Nil,
    /// Push bool.
    Bool(bool),
    /// Pop and discard.
    Pop,
    /// Push local slot.
    GetLocal(u32),
    /// Pop into local slot.
    SetLocal(u32),
    /// Push captured value.
    GetUpvalue(u32),
    /// Pop into captured value.
    SetUpvalue(u32),
    /// Push a global by constant-name index (stdlib → host fallback).
    GetGlobal(u32),
    /// Build list of top `n` values.
    List(u32),
    /// Build map of top `2n` values (key, value pairs).
    Map(u32),
    /// Build an interpolated string from the top `n` values.
    Interp(u32),
    /// Build a range from top two values (`inclusive` flag).
    Range(bool),
    /// Unary op.
    Unary(surf_syntax::ast::UnaryOp),
    /// Binary op.
    Binary(surf_syntax::ast::BinaryOp),
    /// Unconditional jump to absolute offset.
    Jump(u32),
    /// Pop; jump if falsy.
    JumpIfFalse(u32),
    /// Peek; jump if falsy (for `and` / `or` short-circuit).
    JumpIfFalseKeep(u32),
    /// Peek; jump if truthy.
    JumpIfTrueKeep(u32),
    /// Call with `positional` positional args and `kwargs` keyword args
    /// (names are pushed as constants before their values).
    Call {
        /// Positional arg count.
        positional: u32,
        /// Keyword arg count.
        kwargs: u32,
    },
    /// Method call `recv.name(args)`; `name` is a constant index.
    CallMethod {
        /// Constant index of the method name.
        name: u32,
        /// Positional arg count.
        positional: u32,
        /// Keyword arg count.
        kwargs: u32,
    },
    /// `recv.name`
    GetField(u32),
    /// `recv.name = v`
    SetField(u32),
    /// `recv[i]`
    GetIndex,
    /// `recv[i] = v`
    SetIndex,
    /// Make a closure from `functions[i]`, capturing per the function's
    /// upvalue descriptors.
    Closure(u32),
    /// Return top of stack.
    Return,
    /// Set up an iterator over top of stack.
    IterStart,
    /// Advance iterator; push next or jump to offset when done.
    IterNext(u32),
    /// `emit` top of stack.
    Emit,
    /// `exit` with top of stack as code (nil → 0).
    Exit,
    /// `spawn`: callee + args as in `Call`, dispatched to `Host::spawn`.
    Spawn {
        /// Positional arg count.
        positional: u32,
        /// Keyword arg count.
        kwargs: u32,
    },
    /// `parallel for`: items, body closure, opts map on stack.
    ParallelFor,
    /// Begin `try` region; `catch` handler at offset.
    TryBegin(u32),
    /// End `try` region.
    TryEnd,
    /// Hand a declaration to the host; operands are on the stack per
    /// `DeclKind`.
    Declare(DeclKind),
}

/// Which declaration `Op::Declare` builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    /// Config block.
    Config,
    /// `fn`.
    Fn,
    /// `task`.
    Task,
    /// `actor`.
    Actor,
    /// `supervisor`.
    Supervisor,
    /// `on …:` handler.
    Handler,
}

/// Describes how a closure captures one value from its enclosing scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpvalueDesc {
    /// `true` → capture the enclosing function's local `index`; `false` →
    /// re-capture the enclosing function's upvalue `index`.
    pub is_local: bool,
    /// Slot index.
    pub index: u32,
}

/// A compiled unit of code.
#[derive(Debug, Clone, Default)]
pub struct Chunk {
    /// Instructions.
    pub code: Vec<Op>,
    /// Constant pool.
    pub constants: Vec<Value>,
    /// One span per instruction (same length as `code`).
    pub spans: Vec<Span>,
    /// Nested functions referenced by `Op::Closure`.
    pub functions: Vec<Rc<Function>>,
}

/// A compiled function.
#[derive(Debug)]
pub struct Function {
    /// Name (`<main>` for the program body, `<lambda>` for lambdas).
    pub name: Rc<str>,
    /// Parameter names in order.
    pub params: Vec<Rc<str>>,
    /// Number of local slots to reserve.
    pub locals: u32,
    /// Upvalue capture descriptors.
    pub upvalues: Vec<UpvalueDesc>,
    /// Code.
    pub chunk: Chunk,
    /// Span of the declaration.
    pub span: Span,
}

/// A whole compiled program.
#[derive(Debug)]
pub struct CompiledProgram {
    /// Name used in diagnostics.
    pub source_name: String,
    /// The program body. Its prologue emits every hoisted declaration.
    pub main: Rc<Function>,
}
