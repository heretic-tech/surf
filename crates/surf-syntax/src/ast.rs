//! Abstract syntax tree for Surf programs.
//!
//! The shapes here are the contract between the parser (task 5) and the
//! bytecode compiler in `surf-vm` (task 6). Every node carries a [`Span`].
//! The language reference is `docs/language.md`.

use crate::span::Span;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// An identifier with its span.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Ident {
    /// The name.
    pub name: String,
    /// Where it appears.
    pub span: Span,
}

/// A whole source file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Program {
    /// Top-level items in source order.
    pub items: Vec<Item>,
    /// Span of the whole file.
    pub span: Span,
}

/// Top-level items. Declarations (`browser:`, `fn`, `task`, `actor`,
/// `supervisor`, `on …:`) are hoisted and registered with the host before the
/// first statement runs; statements execute in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Item {
    /// `browser:` / `browser work:` config block (declarative; nothing launches).
    Config(ConfigBlock),
    /// `fn name(params):`
    Fn(FnDecl),
    /// `task name(params):` with leading property lines.
    Task(TaskDecl),
    /// `actor Name(params):` with leading property lines.
    Actor(ActorDecl),
    /// `supervisor Name:` with property lines and `spawn` body.
    Supervisor(SupervisorDecl),
    /// `on event(args):` reactive handler.
    Handler(HandlerDecl),
    /// Any other statement.
    Stmt(Stmt),
}

/// `key: value` line inside a config / task / actor / supervisor /
/// `parallel for` header region.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Property {
    /// Property key.
    pub key: Ident,
    /// Property value (any expression; evaluated once at declaration time
    /// for config blocks, at call time for task/actor props).
    pub value: Expr,
    /// Span of the whole line.
    pub span: Span,
}

/// `browser:` or `browser <alias>:` block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigBlock {
    /// The config kind — `browser` in v0.1.
    pub kind: Ident,
    /// Optional alias (`browser work:` → `work`).
    pub alias: Option<Ident>,
    /// Property lines.
    pub props: Vec<Property>,
    /// Span of the whole block.
    pub span: Span,
}

/// A function / task / actor parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    /// Parameter name.
    pub name: Ident,
    /// Optional default value (`fn f(a, b: 2):`).
    pub default: Option<Expr>,
    /// Span.
    pub span: Span,
}

/// `fn name(params):` declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FnDecl {
    /// Function name.
    pub name: Ident,
    /// Parameters.
    pub params: Vec<Param>,
    /// Body block.
    pub body: Block,
    /// Span.
    pub span: Span,
}

/// `task name(params):` declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDecl {
    /// Task name.
    pub name: Ident,
    /// Parameters.
    pub params: Vec<Param>,
    /// Leading property lines (`retry: 5`, `on_fail: shift_proxy()`,
    /// `timeout: 60s`, `fresh: true`).
    pub props: Vec<Property>,
    /// Statements after the properties.
    pub body: Block,
    /// Span.
    pub span: Span,
}

/// `actor Name(params):` declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActorDecl {
    /// Actor name.
    pub name: Ident,
    /// Parameters.
    pub params: Vec<Param>,
    /// Leading property lines.
    pub props: Vec<Property>,
    /// Body; may contain `on message:` handlers and statements.
    pub body: Block,
    /// Span.
    pub span: Span,
}

/// `supervisor Name:` declaration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SupervisorDecl {
    /// Supervisor name.
    pub name: Ident,
    /// `strategy: one_for_one|one_for_all`, `max_restarts: 3`, `within: 60s`.
    pub props: Vec<Property>,
    /// Body: `spawn …` lines and `parallel for …: spawn …` only.
    pub body: Block,
    /// Span.
    pub span: Span,
}

/// `on <event>(<args>):` handler block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HandlerDecl {
    /// Event name: `element_appears`, `navigation`, `dialog`, `request`,
    /// `response`, `message`.
    pub event: Ident,
    /// Handler arguments (selector / URL pattern), may be empty.
    pub args: Vec<Expr>,
    /// Body; the event payload is bound to `event` inside.
    pub body: Block,
    /// Span.
    pub span: Span,
}

/// An indented block of statements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    /// Statements in order.
    pub stmts: Vec<Stmt>,
    /// Span.
    pub span: Span,
}

/// A statement with span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stmt {
    /// Statement payload.
    pub kind: StmtKind,
    /// Span.
    pub span: Span,
}

/// Statement kinds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StmtKind {
    /// Expression statement (usually a call).
    Expr(Expr),
    /// `target = value`. `target` is an `Ident`, `Field` or `Index` expression.
    Assign {
        /// Assignment target.
        target: Expr,
        /// Assigned value.
        value: Expr,
    },
    /// `if … elif … else …`.
    If {
        /// `(condition, block)` for the `if` and each `elif`.
        branches: Vec<(Expr, Block)>,
        /// Optional `else` block.
        else_block: Option<Block>,
    },
    /// `for x in xs:`.
    For {
        /// Loop variable.
        var: Ident,
        /// Iterated expression (list, range, map, string).
        iter: Expr,
        /// Body.
        body: Block,
    },
    /// `parallel for x in xs:` with optional leading `limit:` / `fail_fast:`
    /// property lines.
    ParallelFor {
        /// Loop variable.
        var: Ident,
        /// Iterated expression.
        iter: Expr,
        /// `limit: N`, `fail_fast: true`.
        opts: Vec<Property>,
        /// Body, run as one task per item.
        body: Block,
    },
    /// `while cond:`.
    While {
        /// Condition.
        cond: Expr,
        /// Body.
        body: Block,
    },
    /// `loop:`.
    Loop {
        /// Body.
        body: Block,
    },
    /// `break`.
    Break,
    /// `continue`.
    Continue,
    /// `return` / `return expr`.
    Return(Option<Expr>),
    /// `try: … catch e: …`.
    Try {
        /// Protected block.
        body: Block,
        /// Optional error binding.
        catch_var: Option<Ident>,
        /// Catch block (required).
        catch_block: Block,
    },
    /// `emit expr` — JSON line to stdout.
    Emit(Expr),
    /// `exit` / `exit(code)`.
    Exit(Option<Expr>),
    /// Nested `fn` declaration (closures capture their environment).
    Fn(FnDecl),
    /// Nested `on …:` handler (allowed inside actor bodies).
    Handler(HandlerDecl),
}

/// An expression with span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Expr {
    /// Expression payload.
    pub kind: ExprKind,
    /// Span.
    pub span: Span,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnaryOp {
    /// `-x`
    Neg,
    /// `not x`
    Not,
}

/// Binary operators, in precedence order from lowest (`Or`) to highest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BinaryOp {
    /// `or` (short-circuit)
    Or,
    /// `and` (short-circuit)
    And,
    /// `==`
    Eq,
    /// `!=`
    NotEq,
    /// `<`
    Lt,
    /// `<=`
    LtEq,
    /// `>`
    Gt,
    /// `>=`
    GtEq,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Rem,
}

/// A call argument: positional or `name: value`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Arg {
    /// Keyword, if any.
    pub name: Option<Ident>,
    /// Value.
    pub value: Expr,
    /// Span.
    pub span: Span,
}

/// Map literal key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MapKey {
    /// Bare identifier key: `{a: 1}`.
    Ident(Ident),
    /// String key: `{"b": 2}` (may interpolate).
    Str(Vec<StrSegment>, Span),
    /// Computed key: `{[expr]: 3}`.
    Expr(Expr),
}

/// One segment of a parsed string literal: literal text or an
/// interpolated expression (`"a {x} b"` → `[Lit("a "), Expr(x), Lit(" b")]`).
/// An empty string has no segments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StrSegment {
    /// Literal text, escapes already resolved.
    Lit(String),
    /// `{expr}` — parsed with the full expression grammar.
    Expr(Expr),
}

/// Lambda body: single expression or block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LambdaBody {
    /// `fn(a): a + 1`
    Expr(Box<Expr>),
    /// `fn(a):` followed by an indented block.
    Block(Block),
}

/// Expression kinds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExprKind {
    /// `nil`
    Nil,
    /// `true` / `false`
    Bool(bool),
    /// Integer literal.
    Int(i64),
    /// Float literal.
    Float(f64),
    /// String literal with `{expr}` interpolation segments.
    Str(Vec<StrSegment>),
    /// Duration literal.
    Duration(Duration),
    /// Variable reference.
    Ident(Ident),
    /// `[a, b, c]`
    List(Vec<Expr>),
    /// `{a: 1, "b": 2}`
    Map(Vec<(MapKey, Expr)>),
    /// `a..b` / `a..=b`
    Range {
        /// Start (inclusive).
        start: Box<Expr>,
        /// End.
        end: Box<Expr>,
        /// `..=` when true.
        inclusive: bool,
    },
    /// Unary operation.
    Unary {
        /// Operator.
        op: UnaryOp,
        /// Operand.
        expr: Box<Expr>,
    },
    /// Binary operation.
    Binary {
        /// Operator.
        op: BinaryOp,
        /// Left operand.
        lhs: Box<Expr>,
        /// Right operand.
        rhs: Box<Expr>,
    },
    /// `callee(args)` — `callee` is any expression (usually an `Ident`).
    Call {
        /// Callee.
        callee: Box<Expr>,
        /// Arguments.
        args: Vec<Arg>,
    },
    /// `receiver.name(args)`
    Method {
        /// Receiver.
        receiver: Box<Expr>,
        /// Method name.
        name: Ident,
        /// Arguments.
        args: Vec<Arg>,
    },
    /// `receiver.name`
    Field {
        /// Receiver.
        receiver: Box<Expr>,
        /// Field name.
        name: Ident,
    },
    /// `receiver[index]`
    Index {
        /// Receiver.
        receiver: Box<Expr>,
        /// Index expression.
        index: Box<Expr>,
    },
    /// `fn(a, b): expr` or block lambda.
    Lambda {
        /// Parameters.
        params: Vec<Param>,
        /// Body.
        body: LambdaBody,
    },
    /// `spawn callee(args)` — runs the call concurrently and evaluates to a
    /// task handle. The operand is always a `Call` or `Method` expression.
    /// As a bare statement (`spawn Scout()`) the handle is discarded.
    Spawn(Box<Expr>),
}
