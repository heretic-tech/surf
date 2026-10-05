//! # surf-vm
//!
//! Bytecode compiler + single-threaded async stack interpreter for Surf.
//!
//! Responsibilities:
//! - [`compiler`]: `surf_syntax::ast::Program` → [`CompiledProgram`] (a
//!   bytecode [`bytecode::Chunk`] per function; declarations are emitted as
//!   `Declare*` ops in the main chunk's prologue so the host sees them before
//!   the first statement runs).
//! - [`vm`]: the interpreter. Every operation that can block (a browser
//!   action, `sleep`, `receive`) is an `async` call into the [`Host`]; the VM
//!   itself never touches IO.
//! - [`value`]: the dynamic [`Value`] type (`Rc`-based; **not** `Send`).
//! - [`host`]: the [`Host`] trait the runtime implements, and the
//!   [`Declaration`]s the compiler hands to it.
//! - [`stdlib`]: host-independent builtins (`len`, `keys`, `json`, string
//!   helpers, …) registered in [`Globals`].
//!
//! Decision 1: bytecode VM, no JIT — scripts are IO-bound, cold start must be
//! < 5 ms, and the crate has to run on `wasm32-unknown-unknown` later.
//!
//! This crate is **wasm32-clean**: no `tokio`, no `std::process`, no
//! `std::fs`. Futures are `futures::future::LocalBoxFuture`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bytecode;
pub mod cancel;
pub mod compiler;
pub mod error;
pub mod host;
pub mod stdlib;
pub mod value;
pub mod vm;

pub use bytecode::{Chunk, CompiledProgram, DeclKind, Function, Op, UpvalueDesc};
pub use cancel::CancelToken;
pub use compiler::{
    compile, compile_with_options, compile_with_source, CompileError, CompileOptions,
};
pub use error::{Cancelled, ExitRequest, RuntimeError};
pub use host::{Declaration, FsOp, Host, Prop};
pub use stdlib::{BuiltinFn, Globals, NativeFn};
pub use value::{format_duration, Args, Closure, NativeObject, Range, Upvalue, Value};
pub use vm::Vm;

/// Convenience re-export so hosts don't need a direct `futures` dependency
/// just for the future type.
pub use futures::future::LocalBoxFuture;
