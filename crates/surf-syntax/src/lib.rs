//! # surf-syntax
//!
//! Front end of the Surf language: source text → tokens → AST → diagnostics.
//!
//! Responsibilities:
//! - [`lexer`]: hand-written lexer that produces `INDENT` / `DEDENT` / `NEWLINE`
//!   tokens from an indentation-based source (spaces only), plus duration
//!   literals (`500ms`, `2s`), interpolated strings (`"hi {name}"`), numbers,
//!   identifiers, keywords and punctuation.
//! - [`parser`]: recursive-descent statement parser with a Pratt expression
//!   parser. Produces [`ast::Program`].
//! - [`ast`]: the syntax tree every downstream crate (`surf-vm` compiler,
//!   `surf fmt`, LSP) compiles against. Every node carries a [`Span`].
//! - [`diagnostics`]: structured errors rendered with `ariadne`.
//!
//! This crate is **wasm32-clean**: no `tokio`, no `std::process`, no
//! `std::fs`. It never touches a browser. The authoritative language
//! reference is `docs/language.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod ast;
pub mod diagnostics;
pub mod lexer;
pub mod parser;
pub mod span;
pub mod token;

pub use ast::Program;
pub use diagnostics::{Diagnostic, Diagnostics, Severity};
pub use span::Span;

/// Parse a complete Surf program.
///
/// `name` is used only for diagnostics (file path or `<stdin>`). Every
/// error found is returned together; the result is `Err` if there was at
/// least one.
pub fn parse(name: &str, source: &str) -> Result<Program, Diagnostics> {
    let tokens = lexer::lex(name, source)?;
    parser::parse_tokens(name, source, tokens)
}

/// Parse a single expression (no statements, no layout) — for the REPL
/// and tests. The whole of `source` must be the expression.
pub fn parse_expr(name: &str, source: &str) -> Result<ast::Expr, Diagnostics> {
    let tokens = lexer::lex_expr(name, source, Span::new(0, source.len() as u32))?;
    parser::Parser::new(name, source, tokens).parse_expression()
}
