//! Token kinds produced by the lexer.

use crate::span::Span;
use serde::{Deserialize, Serialize};

/// A lexed token with its span.
#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    /// What kind of token this is (and its payload, if any).
    pub kind: TokenKind,
    /// Source location.
    pub span: Span,
}

/// One piece of an interpolated string literal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Every reserved keyword, for `did you mean` suggestions.
pub const KEYWORDS: &[&str] = &[
    "if",
    "elif",
    "else",
    "for",
    "in",
    "while",
    "loop",
    "break",
    "continue",
    "fn",
    "return",
    "try",
    "catch",
    "and",
    "or",
    "not",
    "true",
    "false",
    "nil",
    "emit",
    "exit",
    "spawn",
    "parallel",
    "task",
    "actor",
    "supervisor",
    "on",
];

impl TokenKind {
    /// Whether this token is a reserved keyword.
    pub fn is_keyword(&self) -> bool {
        self.keyword_text().is_some()
    }

    /// The source text of a keyword token.
    pub fn keyword_text(&self) -> Option<&'static str> {
        Some(match self {
            TokenKind::If => "if",
            TokenKind::Elif => "elif",
            TokenKind::Else => "else",
            TokenKind::For => "for",
            TokenKind::In => "in",
            TokenKind::While => "while",
            TokenKind::Loop => "loop",
            TokenKind::Break => "break",
            TokenKind::Continue => "continue",
            TokenKind::Fn => "fn",
            TokenKind::Return => "return",
            TokenKind::Try => "try",
            TokenKind::Catch => "catch",
            TokenKind::And => "and",
            TokenKind::Or => "or",
            TokenKind::Not => "not",
            TokenKind::True => "true",
            TokenKind::False => "false",
            TokenKind::Nil => "nil",
            TokenKind::Emit => "emit",
            TokenKind::Exit => "exit",
            TokenKind::Spawn => "spawn",
            TokenKind::Parallel => "parallel",
            TokenKind::Task => "task",
            TokenKind::Actor => "actor",
            TokenKind::Supervisor => "supervisor",
            TokenKind::On => "on",
            _ => return None,
        })
    }

    /// Human description for diagnostics: `` identifier `x` ``, `` `(` ``,
    /// `end of line`, …
    pub fn describe(&self) -> String {
        if let Some(kw) = self.keyword_text() {
            return format!("keyword `{kw}`");
        }
        match self {
            TokenKind::Indent => "an indented block".into(),
            TokenKind::Dedent => "end of block".into(),
            TokenKind::Newline => "end of line".into(),
            TokenKind::Eof => "end of file".into(),
            TokenKind::Ident(name) => format!("identifier `{name}`"),
            TokenKind::Int(v) => format!("integer `{v}`"),
            TokenKind::Float(v) => format!("float `{v}`"),
            TokenKind::Str(_) => "string literal".into(),
            TokenKind::Duration(_) => "duration literal".into(),
            other => format!("`{}`", other.punct_text().unwrap_or("?")),
        }
    }

    /// Source text of a punctuation token.
    pub fn punct_text(&self) -> Option<&'static str> {
        Some(match self {
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::LBracket => "[",
            TokenKind::RBracket => "]",
            TokenKind::LBrace => "{",
            TokenKind::RBrace => "}",
            TokenKind::Comma => ",",
            TokenKind::Colon => ":",
            TokenKind::Dot => ".",
            TokenKind::DotDot => "..",
            TokenKind::DotDotEq => "..=",
            TokenKind::Eq => "=",
            TokenKind::EqEq => "==",
            TokenKind::NotEq => "!=",
            TokenKind::Lt => "<",
            TokenKind::LtEq => "<=",
            TokenKind::Gt => ">",
            TokenKind::GtEq => ">=",
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            _ => return None,
        })
    }

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
