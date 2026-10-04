//! Recursive-descent statement parser + Pratt expression parser.
//!
//! Grammar: `docs/language.md`. Precedence (lowest → highest):
//! `or` < `and` < `not` < comparison (`== != < <= > >=`) < `..`/`..=`
//! < `+ -` < `* / %` < unary `-` < postfix (call, method, field, index).

use crate::ast::Program;
use crate::diagnostics::Diagnostics;
use crate::token::Token;

/// Parse a token stream (from [`crate::lexer::lex`]) into a [`Program`].
///
/// `source` is needed to re-lex `{expr}` string-interpolation parts.
pub fn parse_tokens(name: &str, source: &str, tokens: Vec<Token>) -> Result<Program, Diagnostics> {
    Parser::new(name, source, tokens).parse_program()
}

/// Parser state. Implemented in task 5.
pub struct Parser<'src> {
    name: &'src str,
    source: &'src str,
    tokens: Vec<Token>,
    pos: usize,
}

impl<'src> Parser<'src> {
    /// Create a parser over `tokens`.
    pub fn new(name: &'src str, source: &'src str, tokens: Vec<Token>) -> Self {
        Self {
            name,
            source,
            tokens,
            pos: 0,
        }
    }

    /// Parse a whole program.
    pub fn parse_program(mut self) -> Result<Program, Diagnostics> {
        let _ = (self.source, self.tokens.len(), self.pos);
        self.pos = 0;
        Err(Diagnostics::unimplemented(self.name, "parser"))
    }
}
