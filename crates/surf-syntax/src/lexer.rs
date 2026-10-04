//! Hand-written lexer with INDENT / DEDENT tracking.
//!
//! Rules (see `docs/language.md` § Lexical structure):
//! - Indentation is spaces only; a tab anywhere in leading whitespace is an error.
//! - A line ending in `:` opens a block; the next non-blank line must be
//!   indented deeper and establishes the block's indentation level.
//! - Blank lines and comment-only lines never affect indentation.
//! - Newlines inside `(…)`, `[…]`, `{…}` are ignored (implicit continuation).
//! - `#` starts a comment to end of line; a leading `#!` shebang is a comment.
//! - Duration literals: a number followed immediately by `ms`, `s`, `m`, `h`.
//! - Strings are lexed into [`StrPart`]s; each `{expr}` part is kept as raw
//!   source (with its span) and lexed again by the parser via
//!   [`lex_expr`].
//!
//! The lexer collects every error it can recover from (unknown characters,
//! bad escapes, bad indentation) and returns them together; the parser only
//! runs on a clean token stream.

use crate::diagnostics::{Diagnostic, Diagnostics};
use crate::span::Span;
use crate::token::{StrPart, Token, TokenKind};
use std::time::Duration;

/// Lex `source` into a token stream ending in `TokenKind::Eof`.
///
/// `name` is used only for diagnostics.
pub fn lex(name: &str, source: &str) -> Result<Vec<Token>, Diagnostics> {
    Lexer::new(name, source).run()
}

/// Lex the sub-range `span` of `source` as a single expression (no
/// indentation, no `NEWLINE`): used for `{expr}` string interpolation.
/// Token spans are relative to the whole `source`.
pub fn lex_expr(name: &str, source: &str, span: Span) -> Result<Vec<Token>, Diagnostics> {
    let mut lx = Lexer::new(name, source);
    lx.pos = span.start as usize;
    lx.end = span.end as usize;
    lx.expr_mode = true;
    lx.at_line_start = false;
    lx.run()
}

/// Lexer state.
pub struct Lexer<'src> {
    source: &'src str,
    pos: usize,
    end: usize,
    tokens: Vec<Token>,
    diags: Diagnostics,
    /// Indentation levels of the enclosing blocks; `[0]` at the top level.
    indent_stack: Vec<usize>,
    /// Open brackets `(`, `[`, `{` with their offsets.
    brackets: Vec<(char, usize)>,
    at_line_start: bool,
    /// The previous logical line ended in `:` — the next one must be deeper.
    pending_block: Option<Span>,
    /// Expression mode: no layout tokens at all (string interpolation).
    expr_mode: bool,
    tab_reported: bool,
}

impl<'src> Lexer<'src> {
    /// Create a lexer over `source`.
    pub fn new(name: &'src str, source: &'src str) -> Self {
        Self {
            source,
            pos: 0,
            end: source.len(),
            tokens: Vec::new(),
            diags: Diagnostics::new(name),
            indent_stack: vec![0],
            brackets: Vec::new(),
            at_line_start: true,
            pending_block: None,
            expr_mode: false,
            tab_reported: false,
        }
    }

    /// Run to completion.
    pub fn run(mut self) -> Result<Vec<Token>, Diagnostics> {
        self.lex_all();
        if self.diags.is_empty() {
            Ok(self.tokens)
        } else {
            Err(self.diags)
        }
    }

    // ---- helpers ------------------------------------------------------

    fn peek(&self) -> Option<char> {
        self.source[self.pos..self.end].chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.source[self.pos..self.end].chars().nth(n)
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += c.len_utf8();
        Some(c)
    }

    fn push(&mut self, kind: TokenKind, start: usize) {
        self.tokens.push(Token {
            kind,
            span: Span::new(start as u32, self.pos as u32),
        });
    }

    fn push_at(&mut self, kind: TokenKind, span: Span) {
        self.tokens.push(Token { kind, span });
    }

    fn error(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    fn last_kind(&self) -> Option<&TokenKind> {
        self.tokens.last().map(|t| &t.kind)
    }

    // ---- main loop ----------------------------------------------------

    fn lex_all(&mut self) {
        loop {
            if self.at_line_start && self.brackets.is_empty() && !self.expr_mode {
                self.handle_indentation();
                if self.pos >= self.end {
                    break;
                }
                if self.at_line_start {
                    // blank or comment-only line was skipped
                    continue;
                }
            }
            let Some(c) = self.peek() else { break };
            let start = self.pos;
            match c {
                ' ' | '\r' | '\t' => {
                    self.bump();
                    continue;
                }
                '#' if !self.expr_mode => {
                    self.skip_comment();
                    continue;
                }
                '\n' => {
                    self.bump();
                    if self.brackets.is_empty() && !self.expr_mode {
                        self.end_line(start);
                    }
                    self.at_line_start = true;
                    continue;
                }
                _ => {}
            }
            // A real token: this physical line is no longer "at its start"
            // (matters on continuation lines inside brackets, so the `\n`
            // after the closing bracket is a NEWLINE, not a blank line).
            self.at_line_start = false;
            match c {
                '"' => self.lex_string(),
                c if c.is_ascii_digit() => self.lex_number(),
                c if c.is_ascii_alphabetic() || c == '_' => self.lex_ident(),
                _ => self.lex_punct(),
            }
        }
        self.finish();
    }

    fn finish(&mut self) {
        if !self.expr_mode {
            // Terminate the last logical line if the file has no trailing newline.
            let needs_newline = !matches!(
                self.last_kind(),
                None | Some(TokenKind::Newline) | Some(TokenKind::Indent) | Some(TokenKind::Dedent)
            );
            if needs_newline && self.brackets.is_empty() {
                self.end_line(self.pos);
            }
            if let Some(colon) = self.pending_block.take() {
                self.error(
                    Diagnostic::error("expected an indented block after ':'", colon)
                        .with_label("this ':' opens a block")
                        .with_help(
                            "the block body must be on the following line(s), indented deeper",
                        ),
                );
            }
        }
        // An unclosed bracket at EOF is usually a consequence of an earlier
        // error (unclosed string, mismatched bracket): only report it alone.
        if let (Some(&(open, at)), true) = (self.brackets.last(), self.diags.is_empty()) {
            let span = Span::new(at as u32, at as u32 + 1);
            self.error(
                Diagnostic::error(format!("unclosed `{open}`"), span)
                    .with_label("opened here")
                    .with_help(format!("add a matching `{}`", closing_for(open))),
            );
        }
        if !self.expr_mode {
            while self.indent_stack.len() > 1 {
                self.indent_stack.pop();
                self.push_at(TokenKind::Dedent, Span::at(self.pos as u32));
            }
        }
        self.push_at(TokenKind::Eof, Span::at(self.pos as u32));
    }

    /// Emit `NEWLINE` for the logical line that just ended and remember
    /// whether it was a block header.
    fn end_line(&mut self, nl_start: usize) {
        let header = self
            .tokens
            .last()
            .filter(|t| t.kind == TokenKind::Colon)
            .map(|t| t.span);
        self.push_at(
            TokenKind::Newline,
            Span::new(nl_start as u32, self.pos as u32),
        );
        self.pending_block = header;
    }

    fn skip_comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.bump();
        }
    }

    /// At the start of a physical line (outside brackets): measure leading
    /// whitespace, skip blank / comment-only lines, emit INDENT / DEDENT.
    fn handle_indentation(&mut self) {
        let line_start = self.pos;
        let mut width = 0usize;
        let mut tab_at = None;
        while let Some(c) = self.peek() {
            match c {
                ' ' => width += 1,
                '\t' => {
                    tab_at.get_or_insert(self.pos);
                    width += 1;
                }
                '\r' => {}
                _ => break,
            }
            self.bump();
        }
        // Blank or comment-only line: no layout effect.
        match self.peek() {
            None => return,
            Some('#') => {
                self.skip_comment();
                self.bump(); // the '\n', if any
                return;
            }
            Some('\n') => {
                self.bump();
                return;
            }
            _ => {}
        }
        if let Some(at) = tab_at {
            if !self.tab_reported {
                self.tab_reported = true;
                self.error(
                    Diagnostic::error(
                        "tabs are not allowed for indentation",
                        Span::new(at as u32, at as u32 + 1),
                    )
                    .with_label("tab character here")
                    .with_help("indent with spaces only (4 per level is conventional)"),
                );
            }
            // Skip the whole line so one tab-indented block yields one error,
            // and forget the pending header so the skip does not cascade.
            self.pending_block = None;
            self.skip_comment();
            self.bump();
            return;
        }
        self.at_line_start = false;
        let ws = Span::new(line_start as u32, self.pos as u32);
        let current = *self
            .indent_stack
            .last()
            .expect("indent stack is never empty");
        let header = self.pending_block.take();
        if width > current {
            if header.is_none() {
                self.error(
                    Diagnostic::error("unexpected indentation", ws)
                        .with_label("this line is indented deeper than the previous one")
                        .with_help("only a line ending in ':' opens a block"),
                );
            }
            self.indent_stack.push(width);
            self.push_at(TokenKind::Indent, ws);
            return;
        }
        if let Some(colon) = header {
            let first = self.peek().map_or(1, char::len_utf8);
            self.error(
                Diagnostic::error("expected an indented block after ':'", colon)
                    .with_label("this ':' opens a block")
                    .with_secondary(
                        Span::new(self.pos as u32, (self.pos + first) as u32),
                        "this line is not indented deeper",
                    )
                    .with_help("indent the block body with spaces, e.g. 4"),
            );
        }
        if width == current {
            return;
        }
        let levels: Vec<String> = self.indent_stack.iter().map(|l| l.to_string()).collect();
        while *self.indent_stack.last().expect("non-empty") > width {
            self.indent_stack.pop();
            self.push_at(TokenKind::Dedent, Span::at(self.pos as u32));
        }
        let top = *self.indent_stack.last().expect("non-empty");
        if top != width {
            self.error(
                Diagnostic::error("unindent does not match any outer indentation level", ws)
                    .with_label(format!("indented by {width} spaces"))
                    .with_help(format!(
                        "the enclosing blocks are indented by {} spaces",
                        levels.join(", ")
                    )),
            );
            // Recover: treat this as a new level so later lines line up.
            self.indent_stack.push(width);
        }
    }

    // ---- identifiers ----------------------------------------------------

    fn lex_ident(&mut self) {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                self.bump();
            } else {
                break;
            }
        }
        let source = self.source;
        let word = &source[start..self.pos];
        let kind = TokenKind::keyword(word).unwrap_or_else(|| TokenKind::Ident(word.to_string()));
        self.push(kind, start);
    }

    // ---- numbers ----------------------------------------------------------

    fn lex_digits(&mut self) {
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == '_' {
                self.bump();
            } else {
                break;
            }
        }
    }

    fn lex_number(&mut self) {
        let start = self.pos;
        let mut is_float = false;
        self.lex_digits();
        if self.peek() == Some('.') && self.peek_at(1).is_some_and(|c| c.is_ascii_digit()) {
            is_float = true;
            self.bump();
            self.lex_digits();
        }
        if matches!(self.peek(), Some('e') | Some('E')) {
            let exp_ok = match self.peek_at(1) {
                Some(c) if c.is_ascii_digit() => true,
                Some('+') | Some('-') => self.peek_at(2).is_some_and(|c| c.is_ascii_digit()),
                _ => false,
            };
            if exp_ok {
                is_float = true;
                self.bump();
                if matches!(self.peek(), Some('+') | Some('-')) {
                    self.bump();
                }
                self.lex_digits();
            }
        }
        let source = self.source;
        let text: String = source[start..self.pos]
            .chars()
            .filter(|&c| c != '_')
            .collect();
        // Suffix: duration unit or junk.
        let suffix_start = self.pos;
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                self.bump();
            } else {
                break;
            }
        }
        let suffix = &source[suffix_start..self.pos];
        let span = Span::new(start as u32, self.pos as u32);
        if suffix.is_empty() {
            if is_float {
                match text.parse::<f64>() {
                    Ok(v) => self.push_at(TokenKind::Float(v), span),
                    Err(_) => self.error(Diagnostic::error("invalid float literal", span)),
                }
            } else {
                match text.parse::<i64>() {
                    Ok(v) => self.push_at(TokenKind::Int(v), span),
                    Err(_) => self.error(
                        Diagnostic::error("integer literal is too large", span)
                            .with_help(format!("the largest integer is {}", i64::MAX)),
                    ),
                }
            }
            return;
        }
        let unit_ms: u64 = match suffix {
            "ms" => 1,
            "s" => 1_000,
            "m" => 60_000,
            "h" => 3_600_000,
            _ => {
                self.error(
                    Diagnostic::error(format!("unknown duration unit `{suffix}`"), span)
                        .with_label("number followed by an unknown suffix")
                        .with_help("duration units are ms, s, m, h (e.g. 500ms, 2s, 3m, 1h)"),
                );
                return;
            }
        };
        let dur = if is_float {
            text.parse::<f64>()
                .ok()
                .and_then(|v| Duration::try_from_secs_f64(v * unit_ms as f64 / 1000.0).ok())
        } else {
            text.parse::<u64>()
                .ok()
                .and_then(|v| v.checked_mul(unit_ms))
                .map(Duration::from_millis)
        };
        match dur {
            Some(d) => self.push_at(TokenKind::Duration(d), span),
            None => self.error(Diagnostic::error("duration literal is too large", span)),
        }
    }

    // ---- strings ----------------------------------------------------------

    fn lex_string(&mut self) {
        let source = self.source;
        let start = self.pos;
        self.bump(); // opening quote
        let mut parts: Vec<StrPart> = Vec::new();
        let mut buf = String::new();
        let flush = |buf: &mut String, parts: &mut Vec<StrPart>| {
            if !buf.is_empty() {
                parts.push(StrPart::Lit(std::mem::take(buf)));
            }
        };
        loop {
            let Some(c) = self.peek() else {
                self.unclosed_string(start);
                return;
            };
            match c {
                '\n' => {
                    self.unclosed_string(start);
                    return;
                }
                '"' => {
                    self.bump();
                    break;
                }
                '\\' => {
                    let esc_start = self.pos;
                    self.bump();
                    let Some(e) = self.peek().filter(|&e| e != '\n') else {
                        self.unclosed_string(start);
                        return;
                    };
                    self.bump();
                    match e {
                        'n' => buf.push('\n'),
                        't' => buf.push('\t'),
                        'r' => buf.push('\r'),
                        '\\' => buf.push('\\'),
                        '"' => buf.push('"'),
                        '{' => buf.push('{'),
                        '}' => buf.push('}'),
                        'u' => {
                            if let Some(ch) = self.lex_unicode_escape(esc_start) {
                                buf.push(ch);
                            }
                        }
                        other => {
                            let span = Span::new(esc_start as u32, self.pos as u32);
                            self.error(
                                Diagnostic::error(
                                    format!("unknown escape sequence `\\{other}`"),
                                    span,
                                )
                                .with_help("valid escapes: \\n \\t \\r \\\\ \\\" \\{ \\} \\u{…}"),
                            );
                        }
                    }
                }
                '{' => {
                    let brace = self.pos;
                    self.bump();
                    let Some(close) = self.scan_interp(brace) else {
                        return;
                    };
                    let raw = &source[self.pos..close];
                    let span = Span::new(self.pos as u32, close as u32);
                    if raw.trim().is_empty() {
                        self.error(
                            Diagnostic::error("empty interpolation `{}`", Span::new(brace as u32, close as u32 + 1))
                                .with_help("write an expression inside the braces, or `\\{\\}` for literal braces"),
                        );
                    } else {
                        flush(&mut buf, &mut parts);
                        parts.push(StrPart::Expr(raw.to_string(), span));
                    }
                    self.pos = close + 1;
                }
                _ => {
                    buf.push(c);
                    self.bump();
                }
            }
        }
        flush(&mut buf, &mut parts);
        self.push(TokenKind::Str(parts), start);
    }

    fn unclosed_string(&mut self, start: usize) {
        let span = Span::new(start as u32, self.pos as u32);
        self.error(
            Diagnostic::error("unclosed string literal", span)
                .with_label("string starts here")
                .with_help("strings cannot span lines; close it with `\"`"),
        );
    }

    /// `\u{XXXX}` — the `\u` has been consumed. Reports and returns `None`
    /// on a malformed escape (the string itself keeps lexing).
    fn lex_unicode_escape(&mut self, esc_start: usize) -> Option<char> {
        let source = self.source;
        let ch = (|| {
            if self.peek() != Some('{') {
                return None;
            }
            self.bump();
            let digits_start = self.pos;
            while self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                self.bump();
            }
            let digits = &source[digits_start..self.pos];
            if self.peek() != Some('}') || digits.is_empty() || digits.len() > 6 {
                return None;
            }
            self.bump();
            u32::from_str_radix(digits, 16)
                .ok()
                .and_then(char::from_u32)
        })();
        if ch.is_none() {
            let span = Span::new(esc_start as u32, self.pos as u32);
            self.error(
                Diagnostic::error("invalid unicode escape", span)
                    .with_help("write it as `\\u{1F600}` (1–6 hex digits in braces)"),
            );
        }
        ch
    }

    /// Find the `}` matching the `{` at `brace` (`self.pos` is just past
    /// it), skipping nested strings and braces. Returns the index of the
    /// closing brace, or reports an error.
    fn scan_interp(&mut self, brace: usize) -> Option<usize> {
        match scan_braces(self.source, self.pos, self.end) {
            Ok(close) => Some(close),
            Err(at) => {
                self.error(
                    Diagnostic::error(
                        "unclosed `{` in string interpolation",
                        Span::new(brace as u32, brace as u32 + 1),
                    )
                    .with_label("interpolation starts here")
                    .with_help("close the `{expr}` with `}` before the end of the line, or write `\\{` for a literal brace"),
                );
                // Skip to end of line so we do not lex the rest as code.
                self.pos = at;
                None
            }
        }
    }

    // ---- punctuation --------------------------------------------------------

    fn lex_punct(&mut self) {
        let start = self.pos;
        let c = self.bump().expect("caller checked a char is present");
        let two = |lx: &mut Self, next: char, yes: TokenKind, no: TokenKind| -> TokenKind {
            if lx.peek() == Some(next) {
                lx.bump();
                yes
            } else {
                no
            }
        };
        let kind = match c {
            '(' | '[' | '{' => {
                self.brackets.push((c, start));
                match c {
                    '(' => TokenKind::LParen,
                    '[' => TokenKind::LBracket,
                    _ => TokenKind::LBrace,
                }
            }
            ')' | ']' | '}' => {
                self.close_bracket(c, start);
                match c {
                    ')' => TokenKind::RParen,
                    ']' => TokenKind::RBracket,
                    _ => TokenKind::RBrace,
                }
            }
            ',' => TokenKind::Comma,
            ':' => TokenKind::Colon,
            '.' => {
                if self.peek() == Some('.') {
                    self.bump();
                    two(self, '=', TokenKind::DotDotEq, TokenKind::DotDot)
                } else {
                    TokenKind::Dot
                }
            }
            '=' => two(self, '=', TokenKind::EqEq, TokenKind::Eq),
            '<' => two(self, '=', TokenKind::LtEq, TokenKind::Lt),
            '>' => two(self, '=', TokenKind::GtEq, TokenKind::Gt),
            '+' => TokenKind::Plus,
            '-' => TokenKind::Minus,
            '*' => TokenKind::Star,
            '/' => TokenKind::Slash,
            '%' => TokenKind::Percent,
            '!' => {
                if self.peek() == Some('=') {
                    self.bump();
                    TokenKind::NotEq
                } else {
                    self.error(
                        Diagnostic::error(
                            "unexpected character `!`",
                            Span::new(start as u32, self.pos as u32),
                        )
                        .with_help("use `not x` for negation and `!=` for inequality"),
                    );
                    return;
                }
            }
            '&' | '|' => {
                let (word, op) = if c == '&' {
                    ("and", "&&")
                } else {
                    ("or", "||")
                };
                if self.peek() == Some(c) {
                    self.bump();
                }
                self.error(
                    Diagnostic::error(
                        format!("unexpected character `{c}`"),
                        Span::new(start as u32, self.pos as u32),
                    )
                    .with_help(format!("Surf spells `{op}` as `{word}`")),
                );
                return;
            }
            ';' => {
                self.error(
                    Diagnostic::error("unexpected `;`", Span::new(start as u32, self.pos as u32))
                        .with_help("Surf has no semicolons; one statement per line"),
                );
                return;
            }
            '\'' => {
                self.error(
                    Diagnostic::error("unexpected `'`", Span::new(start as u32, self.pos as u32))
                        .with_help("strings use double quotes: \"…\""),
                );
                // Skip to the matching quote on this line to avoid a cascade.
                while let Some(c) = self.peek() {
                    if c == '\n' {
                        break;
                    }
                    self.bump();
                    if c == '\'' {
                        break;
                    }
                }
                return;
            }
            other => {
                self.error(Diagnostic::error(
                    format!("unexpected character `{}`", other.escape_default()),
                    Span::new(start as u32, self.pos as u32),
                ));
                return;
            }
        };
        self.push(kind, start);
    }

    fn close_bracket(&mut self, c: char, at: usize) {
        let span = Span::new(at as u32, at as u32 + 1);
        match self.brackets.pop() {
            None => self.error(
                Diagnostic::error(format!("unmatched `{c}`"), span)
                    .with_label("no matching opening bracket"),
            ),
            Some((open, open_at)) if closing_for(open) != c => {
                self.error(
                    Diagnostic::error(
                        format!(
                            "mismatched closing bracket: expected `{}`, found `{c}`",
                            closing_for(open)
                        ),
                        span,
                    )
                    .with_secondary(
                        Span::new(open_at as u32, open_at as u32 + 1),
                        format!("`{open}` opened here"),
                    ),
                );
            }
            Some(_) => {}
        }
    }
}

fn closing_for(open: char) -> char {
    match open {
        '(' => ')',
        '[' => ']',
        _ => '}',
    }
}

/// Scan forward from `from` (just inside a `{`) to the matching `}`,
/// skipping nested strings (which may themselves interpolate). `Ok(index of
/// the closing brace)`, or `Err(index where the line / input ended)`.
fn scan_braces(source: &str, from: usize, end: usize) -> Result<usize, usize> {
    let mut depth = 1usize;
    let mut i = from;
    while i < end {
        let c = source[i..].chars().next().expect("in bounds");
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(i);
                }
            }
            '"' => {
                i = skip_string(source, i, end)?;
                continue;
            }
            '\n' => return Err(i),
            _ => {}
        }
        i += c.len_utf8();
    }
    Err(end)
}

/// Skip a string literal starting at the quote at `i`; returns the index
/// just past its closing quote.
fn skip_string(source: &str, i: usize, end: usize) -> Result<usize, usize> {
    let mut i = i + 1;
    while i < end {
        let c = source[i..].chars().next().expect("in bounds");
        match c {
            '\\' => {
                i += 1;
                if i < end {
                    i += source[i..].chars().next().map(char::len_utf8).unwrap_or(1);
                }
                continue;
            }
            '"' => return Ok(i + 1),
            '{' => {
                i = scan_braces(source, i + 1, end)? + 1;
                continue;
            }
            '\n' => return Err(i),
            _ => {}
        }
        i += c.len_utf8();
    }
    Err(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex("<test>", src)
            .unwrap_or_else(|d| panic!("{}", d.render(src, false)))
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn layout_tokens() {
        use TokenKind::*;
        let got = kinds("if x:\n    a()\n\n    # comment\n    b()\nc()\n");
        let want = vec![
            If,
            Ident("x".into()),
            Colon,
            Newline,
            Indent,
            Ident("a".into()),
            LParen,
            RParen,
            Newline,
            Ident("b".into()),
            LParen,
            RParen,
            Newline,
            Dedent,
            Ident("c".into()),
            LParen,
            RParen,
            Newline,
            Eof,
        ];
        assert_eq!(got, want);
    }

    #[test]
    fn eof_closes_blocks() {
        use TokenKind::*;
        let got = kinds("loop:\n    if x:\n        break");
        assert_eq!(
            got,
            vec![
                Loop,
                Colon,
                Newline,
                Indent,
                If,
                Ident("x".into()),
                Colon,
                Newline,
                Indent,
                Break,
                Newline,
                Dedent,
                Dedent,
                Eof
            ]
        );
    }

    #[test]
    fn numbers_and_durations() {
        use TokenKind::*;
        assert_eq!(
            kinds("1 1_000 1.5 2e3 500ms 2s 3m 1h 1.5s 1..5"),
            vec![
                Int(1),
                Int(1000),
                Float(1.5),
                Float(2000.0),
                Duration(std::time::Duration::from_millis(500)),
                Duration(std::time::Duration::from_secs(2)),
                Duration(std::time::Duration::from_secs(180)),
                Duration(std::time::Duration::from_secs(3600)),
                Duration(std::time::Duration::from_millis(1500)),
                Int(1),
                DotDot,
                Int(5),
                Newline,
                Eof
            ]
        );
    }

    #[test]
    fn strings_and_interpolation() {
        use TokenKind::*;
        let got = kinds(r#""a {x + 1} b\n{"q{y}"}\{""#);
        assert_eq!(
            got,
            vec![
                Str(vec![
                    StrPart::Lit("a ".into()),
                    StrPart::Expr("x + 1".into(), Span::new(4, 9)),
                    StrPart::Lit(" b\n".into()),
                    StrPart::Expr("\"q{y}\"".into(), Span::new(15, 21)),
                    StrPart::Lit("{".into()),
                ]),
                Newline,
                Eof
            ]
        );
    }

    #[test]
    fn brackets_continue_lines() {
        use TokenKind::*;
        let got = kinds("f(\n  1,\n  2,\n)\n");
        assert_eq!(
            got,
            vec![
                Ident("f".into()),
                LParen,
                Int(1),
                Comma,
                Int(2),
                Comma,
                RParen,
                Newline,
                Eof
            ]
        );
    }

    #[test]
    fn errors_are_collected() {
        let err = lex("<t>", "x = 1 @ 2\ny = 3 $ 4\n").unwrap_err();
        assert_eq!(err.len(), 2);
        assert!(err.items[0].message.contains('@'));
        assert!(err.items[1].message.contains('$'));
    }

    #[test]
    fn expr_mode_has_no_layout() {
        let src = "\"{x}\"";
        let toks = lex_expr("<t>", src, Span::new(2, 3)).unwrap();
        assert_eq!(toks.len(), 2);
        assert_eq!(toks[0].kind, TokenKind::Ident("x".into()));
        assert_eq!(toks[0].span, Span::new(2, 3));
        assert_eq!(toks[1].kind, TokenKind::Eof);
    }
}
