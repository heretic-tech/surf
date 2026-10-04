//! Hand-written lexer with INDENT / DEDENT tracking.
//!
//! Rules (see `docs/language.md` § Lexical structure):
//! - Indentation is spaces only; a tab anywhere in leading whitespace is an error.
//! - A line ending in `:` opens a block; the next non-blank line must be
//!   indented deeper and establishes the block's indentation level.
//! - Blank lines and comment-only lines never affect indentation.
//! - Newlines inside `(…)`, `[…]`, `{…}` are ignored (implicit continuation).
//! - `#` starts a comment to end of line; a leading `#!` shebang is a comment.
//! - Duration literals: integer followed immediately by `ms`, `s`, `m`, `h`.

use crate::diagnostics::Diagnostics;
use crate::token::Token;

/// Lex `source` into a token stream ending in `TokenKind::Eof`.
///
/// `name` is used only for diagnostics.
pub fn lex(name: &str, source: &str) -> Result<Vec<Token>, Diagnostics> {
    Lexer::new(name, source).run()
}

/// Lexer state. Implemented in task 5 (`surf-syntax: lexer, parser, AST,
/// diagnostics`).
pub struct Lexer<'src> {
    name: &'src str,
    source: &'src str,
}

impl<'src> Lexer<'src> {
    /// Create a lexer over `source`.
    pub fn new(name: &'src str, source: &'src str) -> Self {
        Self { name, source }
    }

    /// Run to completion.
    pub fn run(self) -> Result<Vec<Token>, Diagnostics> {
        let _ = self.source;
        Err(Diagnostics::unimplemented(self.name, "lexer"))
    }
}
