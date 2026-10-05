//! Bytecode representation.
//!
//! A straightforward stack machine. Every instruction is a small `Copy`
//! enum; operands that are not inline (names, strings, numbers) live in the
//! chunk's constant pool and are referenced by index.
//!
//! Call convention (see `docs/architecture.md`, "Keyword arguments"): the
//! callee is pushed first, then the positional arguments in order, then one
//! `(name, value)` pair per keyword argument — the name as a string
//! constant. Keyword arguments therefore always *trail* the positionals on
//! the stack and the `Call*` instruction carries both counts.

use crate::value::Value;
use std::rc::Rc;
use surf_syntax::Span;

/// Stack-VM opcodes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Op {
    /// Push `constants[i]`.
    Const(u32),
    /// Push `nil`.
    Nil,
    /// Push bool.
    Bool(bool),
    /// Pop and discard.
    Pop,
    /// Duplicate the top of stack.
    Dup,
    /// Duplicate the top two values (`a b` → `a b a b`; compound index
    /// assignment evaluates receiver and index once).
    Dup2,
    /// Push local slot.
    GetLocal(u32),
    /// Pop into local slot.
    SetLocal(u32),
    /// Push captured value.
    GetUpvalue(u32),
    /// Pop into captured value.
    SetUpvalue(u32),
    /// Push a global by constant-name index (host → stdlib fallback).
    GetGlobal(u32),
    /// Pop into a host global by constant-name index (`Host::set_global`);
    /// emitted for top-level assignments under
    /// `CompileOptions::top_level_globals` (the REPL).
    SetGlobal(u32),
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
    /// Unconditional forward jump to absolute offset.
    Jump(u32),
    /// Unconditional backward jump (loop back-edge; checks cancellation).
    Loop(u32),
    /// Pop; jump if falsy.
    JumpIfFalse(u32),
    /// Peek; jump if falsy (for `and` short-circuit).
    JumpIfFalseKeep(u32),
    /// Peek; jump if truthy (for `or` short-circuit).
    JumpIfTrueKeep(u32),
    /// Call a value with `positional` positional args and `kwargs` keyword
    /// args (names are pushed as constants before their values).
    Call {
        /// Positional arg count.
        positional: u32,
        /// Keyword arg count.
        kwargs: u32,
    },
    /// Call a global by name (constant index) — user `fn`s, stdlib, bare
    /// browser actions via `Host::call_global`. No callee on the stack.
    CallGlobal {
        /// Constant index of the name.
        name: u32,
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
    /// `recv.name = v` (stack: recv, v).
    SetField(u32),
    /// `recv[i]`
    GetIndex,
    /// `recv[i] = v` (stack: recv, i, v).
    SetIndex,
    /// Make a closure from `functions[i]`, capturing per the function's
    /// upvalue descriptors.
    Closure(u32),
    /// If parameter `slot` was not supplied by the caller, fall through to
    /// the default-value code; otherwise jump to `skip`.
    ParamDefault {
        /// Parameter slot.
        slot: u32,
        /// Where to continue when the argument was supplied.
        skip: u32,
    },
    /// Return top of stack.
    Return,
    /// Pop an iterable and start iterating it (pushed on the frame's
    /// iterator stack).
    IterStart,
    /// Advance the innermost iterator; push the next item or jump to the
    /// offset when exhausted (the iterator is dropped on exhaustion).
    IterNext(u32),
    /// Drop the innermost iterator (used by `break`).
    IterEnd,
    /// `emit` top of stack.
    Emit,
    /// `exit` with top of stack as code (nil → 0).
    Exit,
    /// `spawn name(args)`: args as in `Call`, dispatched to `Host::spawn`.
    Spawn {
        /// Constant index of the callee name.
        name: u32,
        /// Positional arg count.
        positional: u32,
        /// Keyword arg count.
        kwargs: u32,
    },
    /// `spawn recv.name(args)`: receiver below the args, dispatched to
    /// `Host::spawn_method`.
    SpawnMethod {
        /// Constant index of the method name.
        name: u32,
        /// Positional arg count.
        positional: u32,
        /// Keyword arg count.
        kwargs: u32,
    },
    /// `spawn callee(args)` where the callee is a value (a closure in a
    /// local, `spawn f()`): callee below the args, dispatched to
    /// `Host::spawn_value`.
    SpawnValue {
        /// Positional arg count.
        positional: u32,
        /// Keyword arg count.
        kwargs: u32,
    },
    /// `parallel for`: items, body closure, then `kwargs` `(name, value)`
    /// option pairs on the stack.
    ParallelFor {
        /// Option count.
        kwargs: u32,
    },
    /// Begin `try` region; `catch` handler at offset.
    TryBegin(u32),
    /// End `try` region (normal exit or early leave).
    TryEnd,
    /// Hand a declaration to the host. Stack layout per [`DeclKind`]:
    /// `props` triples `(name, value, lazy: bool)` followed by the body
    /// closure (`Fn`/`Task`/`Actor`/`Supervisor`), the props map (`Config`)
    /// or `props` handler args + body closure (`Handler`).
    Declare {
        /// Declaration kind.
        kind: DeclKind,
        /// Constant index of the name (event name for handlers; `browser`
        /// for config blocks).
        name: u32,
        /// Constant index of the alias (config) or `u32::MAX`.
        alias: u32,
        /// Property / argument count.
        props: u32,
    },
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
    /// Names of the local slots (parameters first), for the `used before
    /// assignment` diagnostic. May be shorter than `Function::locals` for
    /// hand-built functions.
    pub local_names: Vec<Rc<str>>,
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
    /// Parameter names in order (they occupy the first local slots).
    pub params: Vec<Rc<str>>,
    /// Which parameters have a default value (same length as `params`).
    pub has_default: Vec<bool>,
    /// Number of local slots to reserve (including parameters).
    pub locals: u32,
    /// Upvalue capture descriptors.
    pub upvalues: Vec<UpvalueDesc>,
    /// Code.
    pub chunk: Chunk,
    /// Span of the declaration.
    pub span: Span,
    /// Byte offset of the start of every source line (shared by every
    /// function of a program); empty when the source was not available.
    pub line_starts: Rc<[u32]>,
}

impl Function {
    /// 1-based line of byte offset `offset`, if the line table is known.
    pub fn line_of(&self, offset: u32) -> Option<u32> {
        if self.line_starts.is_empty() {
            return None;
        }
        let idx = self.line_starts.partition_point(|&s| s <= offset);
        Some(idx as u32)
    }
}

/// A whole compiled program.
#[derive(Debug)]
pub struct CompiledProgram {
    /// Name used in diagnostics.
    pub source_name: String,
    /// The program body. Its prologue emits every hoisted declaration.
    pub main: Rc<Function>,
}
