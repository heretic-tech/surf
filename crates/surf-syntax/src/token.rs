//! Token kinds produced by the lexer.

use crate::span::Span;

/// A lexed token with its span.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// What kind of token this is (and its payload, if any).
    pub kind: TokenKind,
    /// Source location.
    pub span: Span,
}

/// One piece of an interpolated string literal.
#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    /// Literal text (escapes already resolved).
    Lit(String),
    /// `{expr}` — raw source of the embedded expression, lexed and parsed
    /// recursively by the parser. `Span` is the span of the expression text.
    Expr(String, Span),
}

/// Token kinds. Keywords are reserved words and cannot be used as identifiers.
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // ---- layout ----
    /// Start of an indented block (after a line ending in `:`).
    Indent,
    /// End of an indented block.
    Dedent,
    /// Logical line end. Suppressed inside `(…)`, `[…]`, `{…}`.
    Newline,
    /// End of input (always emitted last, after any pending `Dedent`s).
    Eof,

    // ---- literals ----
    /// Identifier (not a keyword).
    Ident(String),
    /// Integer literal.
    Int(i64),
    /// Float literal.
    Float(f64),
    /// String literal with interpolation parts.
    Str(Vec<StrPart>),
    /// Duration literal in milliseconds-precision units (`500ms`, `2s`, `3m`, `1h`).
    Duration(std::time::Duration),

    // ---- keywords ----
    /// `if`
    If,
    /// `elif`
    Elif,
    /// `else`
    Else,
    /// `for`
    For,
    /// `in`
    In,
    /// `while`
    While,
    /// `loop`
    Loop,
    /// `break`
    Break,
    /// `continue`
    Continue,
    /// `fn`
    Fn,
    /// `return`
    Return,
    /// `try`
    Try,
    /// `catch`
    Catch,
    /// `and`
    And,
    /// `or`
    Or,
    /// `not`
    Not,
    /// `true`
    True,
    /// `false`
    False,
    /// `nil`
    Nil,
    /// `emit`
    Emit,
    /// `exit`
    Exit,
    /// `spawn`
    Spawn,
    /// `parallel`
    Parallel,
    /// `task`
    Task,
    /// `actor`
    Actor,
    /// `supervisor`
    Supervisor,
    /// `on`
    On,

    // ---- punctuation / operators ----
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `[`
    LBracket,
    /// `]`
    RBracket,
    /// `{`
    LBrace,
    /// `}`
    RBrace,
    /// `,`
    Comma,
    /// `:`
    Colon,
    /// `.`
    Dot,
    /// `..`
    DotDot,
    /// `..=`
    DotDotEq,
    /// `=`
    Eq,
    /// `==`
    EqEq,
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
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
}

impl TokenKind {
    /// Map a word to its keyword token, if it is one.
    pub fn keyword(word: &str) -> Option<TokenKind> {
        Some(match word {
            "if" => TokenKind::If,
            "elif" => TokenKind::Elif,
            "else" => TokenKind::Else,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "while" => TokenKind::While,
            "loop" => TokenKind::Loop,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "fn" => TokenKind::Fn,
            "return" => TokenKind::Return,
            "try" => TokenKind::Try,
            "catch" => TokenKind::Catch,
            "and" => TokenKind::And,
            "or" => TokenKind::Or,
            "not" => TokenKind::Not,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "nil" => TokenKind::Nil,
            "emit" => TokenKind::Emit,
            "exit" => TokenKind::Exit,
            "spawn" => TokenKind::Spawn,
            "parallel" => TokenKind::Parallel,
            "task" => TokenKind::Task,
            "actor" => TokenKind::Actor,
            "supervisor" => TokenKind::Supervisor,
            "on" => TokenKind::On,
            _ => return None,
        })
    }
}
