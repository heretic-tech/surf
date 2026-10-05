//! Recursive-descent statement parser + Pratt expression parser.
//!
//! Grammar: `docs/language.md` § 9. Precedence (lowest → highest):
//! `or` < `and` < `not` < comparison (`== != < <= > >=`, non-associative)
//! < `..`/`..=` (non-associative) < `+ -` < `* / %` < unary `-` < postfix
//! (call, method, field, index). `spawn` is a prefix on a postfix chain.
//!
//! ## Property lines
//!
//! A line of the shape `IDENT ":" …` is a *property line*. Where it is
//! permitted — a `browser:` body, or the leading region of a `task`,
//! `actor`, `supervisor` or `parallel for` body — it is parsed as a
//! [`Property`]. Once the first statement of such a body has been seen,
//! a further `IDENT ":"` line is the error `property lines must come before
//! statements`. Anywhere else (top level, `if`/`for`/`fn` bodies, …) it is
//! a syntax error. A property with no value on its line (`proxy:` followed
//! by an indented block of `key: value` lines) is a nested map.
//!
//! ## Recovery
//!
//! Every error is recorded and parsing resumes at the next logical line
//! (skipping any block the broken line opened), so one run reports several
//! independent mistakes. The result is `Err` whenever anything was
//! recorded.

use crate::ast::{
    ActorDecl, Arg, BinaryOp, Block, ConfigBlock, Expr, ExprKind, FnDecl, HandlerDecl, Ident, Item,
    LambdaBody, MapKey, Param, Program, Property, Stmt, StmtKind, StrSegment, SupervisorDecl,
    TaskDecl, UnaryOp,
};
use crate::diagnostics::{did_you_mean, suggest, Diagnostic, Diagnostics};
use crate::lexer;
use crate::span::Span;
use crate::token::{StrPart, Token, TokenKind, KEYWORDS};

/// Keys accepted in a `browser:` block.
pub const BROWSER_PROPS: &[&str] = &[
    "path",
    "cdp",
    "pool",
    "proxy",
    "proxies",
    "headless",
    "virtual",
    "size",
    "profile",
    "flags",
    "timeout",
    "engine",
    "downloads",
];
/// Properties accepted at the top of a `task` (and `actor`) body.
pub const TASK_PROPS: &[&str] = &["retry", "on_fail", "timeout", "fresh"];
/// Properties accepted at the top of a `supervisor` body.
pub const SUPERVISOR_PROPS: &[&str] = &["strategy", "max_restarts", "within"];
/// Options accepted at the top of a `parallel for` body.
pub const PARALLEL_FOR_PROPS: &[&str] = &["limit", "fail_fast"];
/// Events an `on …:` handler may subscribe to. `intercept` is also
/// spelled without `on`: `intercept(pattern):` at the top level.
pub const HANDLER_EVENTS: &[&str] = &[
    "element_appears",
    "navigation",
    "dialog",
    "request",
    "response",
    "intercept",
    "message",
];

/// Parse a token stream (from [`crate::lexer::lex`]) into a [`Program`].
///
/// `source` is needed to re-lex `{expr}` string-interpolation parts.
pub fn parse_tokens(name: &str, source: &str, tokens: Vec<Token>) -> Result<Program, Diagnostics> {
    Parser::new(name, source, tokens).parse_program()
}

/// Marker: a diagnostic has already been recorded; unwind to the nearest
/// statement boundary and resynchronise there.
#[derive(Debug, Clone, Copy)]
struct Fail;

type PResult<T> = Result<T, Fail>;

/// What kind of body a statement is being parsed in; drives the
/// property-line and handler rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctx {
    TopLevel,
    /// `if` / `for` / `fn` / lambda / handler bodies.
    Block,
    Task,
    Actor,
    Supervisor,
    ParallelFor,
}

impl Ctx {
    fn takes_props(self) -> bool {
        matches!(
            self,
            Ctx::Task | Ctx::Actor | Ctx::Supervisor | Ctx::ParallelFor
        )
    }

    fn noun(self) -> &'static str {
        match self {
            Ctx::TopLevel => "the top level",
            Ctx::Block => "this block",
            Ctx::Task => "the task body",
            Ctx::Actor => "the actor body",
            Ctx::Supervisor => "the supervisor body",
            Ctx::ParallelFor => "the `parallel for` body",
        }
    }
}

const EOF: TokenKind = TokenKind::Eof;

/// Parser state.
pub struct Parser<'src> {
    name: &'src str,
    source: &'src str,
    tokens: Vec<Token>,
    pos: usize,
    diags: Diagnostics,
    /// End offset of the last non-layout token consumed (for node spans).
    last_end: u32,
    /// Span of the first unnamed `browser:` block, to flag a second one.
    unnamed_browser: Option<Span>,
    /// Parsing a `{expr}` interpolation: `Eof` is really `}`.
    in_interp: bool,
}

impl<'src> Parser<'src> {
    /// Create a parser over `tokens`.
    pub fn new(name: &'src str, source: &'src str, tokens: Vec<Token>) -> Self {
        let last_end = tokens.first().map(|t| t.span.start).unwrap_or(0);
        Self {
            name,
            source,
            tokens,
            pos: 0,
            diags: Diagnostics::new(name),
            last_end,
            unnamed_browser: None,
            in_interp: false,
        }
    }

    /// Parse a whole program.
    pub fn parse_program(mut self) -> Result<Program, Diagnostics> {
        let mut items = Vec::new();
        while !self.at_eof() {
            let before = self.pos;
            match self.parse_item() {
                Ok(item) => items.push(item),
                Err(Fail) => self.sync(),
            }
            if self.pos == before {
                self.advance();
            }
        }
        if self.diags.is_empty() {
            Ok(Program {
                items,
                span: Span::new(0, self.source.len() as u32),
            })
        } else {
            Err(self.diags)
        }
    }

    /// Parse a single expression that must consume the whole token stream
    /// (used for string interpolation and the REPL).
    pub fn parse_expression(mut self) -> Result<Expr, Diagnostics> {
        let result = self.parse_expr().and_then(|e| {
            while self.at(&TokenKind::Newline) {
                self.advance();
            }
            if self.at_eof() {
                Ok(e)
            } else {
                let found = self.describe_current();
                Err(self.err(Diagnostic::error(
                    format!("expected end of expression, found {found}"),
                    self.span(),
                )))
            }
        });
        match result {
            Ok(e) if self.diags.is_empty() => Ok(e),
            _ => Err(self.diags),
        }
    }

    // ---- token access -------------------------------------------------------

    fn peek(&self) -> &TokenKind {
        self.tokens.get(self.pos).map(|t| &t.kind).unwrap_or(&EOF)
    }

    fn peek_n(&self, n: usize) -> &TokenKind {
        self.tokens
            .get(self.pos + n)
            .map(|t| &t.kind)
            .unwrap_or(&EOF)
    }

    fn span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map(|t| t.span)
            .unwrap_or_else(|| Span::at(self.source.len() as u32))
    }

    fn at(&self, kind: &TokenKind) -> bool {
        self.peek() == kind
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), TokenKind::Eof)
    }

    fn at_ident(&self) -> bool {
        matches!(self.peek(), TokenKind::Ident(_))
    }

    /// `IDENT ":"` at the current position — a property line or keyword arg.
    fn at_prop_line(&self) -> bool {
        self.at_ident() && matches!(self.peek_n(1), TokenKind::Colon)
    }

    fn prev_is_dedent(&self) -> bool {
        self.pos > 0 && matches!(self.tokens[self.pos - 1].kind, TokenKind::Dedent)
    }

    fn advance(&mut self) -> Token {
        let tok = self.tokens[self.pos.min(self.tokens.len() - 1)].clone();
        if !matches!(tok.kind, TokenKind::Eof) {
            self.pos += 1;
        }
        if !matches!(
            tok.kind,
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent | TokenKind::Eof
        ) {
            self.last_end = tok.span.end;
        }
        tok
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: &TokenKind, what: &str) -> PResult<Span> {
        if self.at(kind) {
            Ok(self.advance().span)
        } else {
            Err(self.unexpected(what))
        }
    }

    fn describe(&self, kind: &TokenKind) -> String {
        match kind {
            TokenKind::Eof if self.in_interp => "end of interpolation".into(),
            other => other.describe(),
        }
    }

    fn describe_current(&self) -> String {
        self.describe(self.peek())
    }

    /// Source text of the current token.
    fn current_text(&self) -> &'src str {
        self.span().slice(self.source)
    }

    // ---- diagnostics ------------------------------------------------------

    fn err(&mut self, d: Diagnostic) -> Fail {
        self.diags.push(d);
        Fail
    }

    fn unexpected(&mut self, expected: &str) -> Fail {
        let found = self.describe_current();
        let mut d = Diagnostic::error(format!("expected {expected}, found {found}"), self.span())
            .with_label(format!("expected {expected}"));
        if let Some(kw) = self.peek().keyword_text() {
            d = d.with_help(format!("`{kw}` is a reserved keyword"));
        }
        self.err(d)
    }

    /// The name of a `fn` / `task` / `actor` / `supervisor` declaration;
    /// `task = 1` gets a keyword-as-variable message.
    fn decl_name(&mut self, keyword: &str, what: &str) -> PResult<Ident> {
        if self.at(&TokenKind::Eq) {
            return Err(self.err(
                Diagnostic::error(
                    format!(
                        "`{keyword}` is a reserved keyword and cannot be used as a variable name"
                    ),
                    self.span(),
                )
                .with_label("cannot assign to a keyword")
                .with_help("pick another name"),
            ));
        }
        self.ident(what)
    }

    /// Skip to the start of the next logical line at the current block
    /// level: consume through the next `NEWLINE`, plus any block the broken
    /// line opened. Stops *before* `DEDENT` / `EOF`.
    fn sync(&mut self) {
        loop {
            match self.peek() {
                TokenKind::Eof | TokenKind::Dedent => return,
                TokenKind::Newline => {
                    self.advance();
                    break;
                }
                TokenKind::Indent => self.skip_block(),
                _ => {
                    self.advance();
                }
            }
        }
        if self.at(&TokenKind::Indent) {
            self.skip_block();
        }
    }

    /// At `INDENT`: skip the balanced block.
    fn skip_block(&mut self) {
        let mut depth = 0usize;
        loop {
            match self.peek() {
                TokenKind::Indent => {
                    depth += 1;
                    self.advance();
                }
                TokenKind::Dedent => {
                    depth = depth.saturating_sub(1);
                    self.advance();
                    if depth == 0 {
                        return;
                    }
                }
                TokenKind::Eof => return,
                _ => {
                    self.advance();
                }
            }
        }
    }

    // ---- items --------------------------------------------------------------

    fn parse_item(&mut self) -> PResult<Item> {
        Ok(match self.peek() {
            TokenKind::Ident(s) if s == "browser" && self.is_config_header() => {
                Item::Config(self.parse_config()?)
            }
            TokenKind::Task => Item::Task(self.parse_task()?),
            TokenKind::Actor => Item::Actor(self.parse_actor()?),
            TokenKind::Supervisor => Item::Supervisor(self.parse_supervisor()?),
            TokenKind::On => Item::Handler(self.parse_handler(Ctx::TopLevel)?),
            TokenKind::Ident(s) if s == "intercept" && self.is_intercept_block() => {
                let event = self.ident("intercept")?;
                Item::Handler(self.parse_handler_rest(event.span, event, Ctx::TopLevel)?)
            }
            TokenKind::Fn if !matches!(self.peek_n(1), TokenKind::LParen) => {
                Item::Fn(self.parse_fn_decl()?)
            }
            _ => Item::Stmt(self.parse_stmt(Ctx::TopLevel)?),
        })
    }

    /// `intercept(` … `):` — a handler block rather than a call statement.
    /// Newlines are suppressed inside the parentheses, so the matching `)`
    /// is followed directly by `:` when this is a block.
    fn is_intercept_block(&self) -> bool {
        if !matches!(self.peek_n(1), TokenKind::LParen) {
            return false;
        }
        let mut depth = 0usize;
        let mut i = self.pos + 1;
        while let Some(t) = self.tokens.get(i) {
            match &t.kind {
                TokenKind::LParen | TokenKind::LBracket | TokenKind::LBrace => depth += 1,
                TokenKind::RParen | TokenKind::RBracket | TokenKind::RBrace => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return matches!(
                            self.tokens.get(i + 1).map(|t| &t.kind),
                            Some(TokenKind::Colon)
                        );
                    }
                }
                TokenKind::Newline | TokenKind::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn is_config_header(&self) -> bool {
        matches!(self.peek_n(1), TokenKind::Colon)
            || (matches!(self.peek_n(1), TokenKind::Ident(_))
                && matches!(self.peek_n(2), TokenKind::Colon))
    }

    fn parse_config(&mut self) -> PResult<ConfigBlock> {
        let kind = self.ident("a config block name")?;
        let alias = if self.at_ident() {
            Some(self.ident("a browser alias")?)
        } else {
            None
        };
        if alias.is_none() {
            if let Some(first) = self.unnamed_browser {
                self.err(
                    Diagnostic::error(
                        "only one unnamed `browser:` block is allowed per file",
                        kind.span,
                    )
                    .with_secondary(first, "the first `browser:` block is here")
                    .with_help("give the second browser a name: `browser work:`"),
                );
            } else {
                self.unnamed_browser = Some(kind.span);
            }
        }
        self.expect_colon("after `browser`")?;
        self.expect_block_start()?;
        let props = self.parse_prop_lines(Some(BROWSER_PROPS), "browser option")?;
        self.expect(&TokenKind::Dedent, "end of block")?;
        Ok(ConfigBlock {
            span: kind.span.merge(Span::at(self.last_end)),
            kind,
            alias,
            props,
        })
    }

    fn parse_fn_decl(&mut self) -> PResult<FnDecl> {
        let start = self.expect(&TokenKind::Fn, "`fn`")?;
        let name = self.decl_name("fn", "a function name")?;
        let params = self.parse_params()?;
        let body = self.parse_block(Ctx::Block, "after the parameter list")?;
        Ok(FnDecl {
            name,
            params,
            body,
            span: start.merge(Span::at(self.last_end)),
        })
    }

    fn parse_task(&mut self) -> PResult<TaskDecl> {
        let start = self.expect(&TokenKind::Task, "`task`")?;
        let name = self.decl_name("task", "a task name")?;
        let params = self.parse_params()?;
        let (props, body) = self.parse_prop_body(
            Ctx::Task,
            Some(TASK_PROPS),
            "task property",
            "after the parameter list",
        )?;
        Ok(TaskDecl {
            name,
            params,
            props,
            body,
            span: start.merge(Span::at(self.last_end)),
        })
    }

    fn parse_actor(&mut self) -> PResult<ActorDecl> {
        let start = self.expect(&TokenKind::Actor, "`actor`")?;
        let name = self.decl_name("actor", "an actor name")?;
        let params = self.parse_params()?;
        let (props, body) = self.parse_prop_body(
            Ctx::Actor,
            Some(TASK_PROPS),
            "actor property",
            "after the parameter list",
        )?;
        Ok(ActorDecl {
            name,
            params,
            props,
            body,
            span: start.merge(Span::at(self.last_end)),
        })
    }

    fn parse_supervisor(&mut self) -> PResult<SupervisorDecl> {
        let start = self.expect(&TokenKind::Supervisor, "`supervisor`")?;
        let name = self.decl_name("supervisor", "a supervisor name")?;
        if self.at(&TokenKind::LParen) {
            self.err(
                Diagnostic::error("supervisors do not take parameters", self.span())
                    .with_help("write `supervisor Name:`"),
            );
            self.parse_params()?;
        }
        let (props, body) = self.parse_prop_body(
            Ctx::Supervisor,
            Some(SUPERVISOR_PROPS),
            "supervisor property",
            "after the supervisor name",
        )?;
        self.check_supervisor_body(&body);
        Ok(SupervisorDecl {
            name,
            props,
            body,
            span: start.merge(Span::at(self.last_end)),
        })
    }

    fn check_supervisor_body(&mut self, body: &Block) {
        for stmt in &body.stmts {
            match &stmt.kind {
                StmtKind::Expr(Expr {
                    kind: ExprKind::Spawn(_),
                    ..
                }) => {}
                StmtKind::ParallelFor { body, .. } => self.check_supervisor_body(body),
                _ => {
                    self.err(
                        Diagnostic::error("unexpected statement in supervisor body", stmt.span)
                            .with_label("not a `spawn` line")
                            .with_help(
                                "a supervisor body may only contain `spawn Child(…)` lines and \
                                 `parallel for … :` blocks of `spawn` lines",
                            ),
                    );
                }
            }
        }
    }

    fn parse_handler(&mut self, ctx: Ctx) -> PResult<HandlerDecl> {
        let start = self.expect(&TokenKind::On, "`on`")?;
        let event = self.ident("an event name")?;
        self.parse_handler_rest(start, event, ctx)
    }

    /// The arguments and body after the event name (`on <event>` or the
    /// bare `intercept` form).
    fn parse_handler_rest(&mut self, start: Span, event: Ident, ctx: Ctx) -> PResult<HandlerDecl> {
        let mut args = Vec::new();
        if self.at(&TokenKind::LParen) {
            for arg in self.parse_call_args()? {
                if let Some(name) = &arg.name {
                    self.err(
                        Diagnostic::error("handler arguments are positional", name.span).with_help(
                            format!("write `on {}(…)` without `{}:`", event.name, name.name),
                        ),
                    );
                }
                args.push(arg.value);
            }
        }
        if !HANDLER_EVENTS.contains(&event.name.as_str()) {
            let help = did_you_mean(&event.name, HANDLER_EVENTS.iter().copied())
                .unwrap_or_else(|| format!("events are: {}", HANDLER_EVENTS.join(", ")));
            self.err(
                Diagnostic::error(format!("unknown event `{}`", event.name), event.span)
                    .with_help(help),
            );
        } else if event.name == "message" && ctx != Ctx::Actor {
            self.err(
                Diagnostic::error(
                    "`on message:` is only valid inside an actor body",
                    event.span,
                )
                .with_help(
                    "messages are delivered to actors; move this handler into an `actor` body",
                ),
            );
        }
        let body = self.parse_block(Ctx::Block, "after the event")?;
        Ok(HandlerDecl {
            event,
            args,
            body,
            span: start.merge(Span::at(self.last_end)),
        })
    }

    // ---- blocks and properties ----------------------------------------------

    fn expect_colon(&mut self, after: &str) -> PResult<Span> {
        if self.at(&TokenKind::Colon) {
            return Ok(self.advance().span);
        }
        let found = self.describe_current();
        Err(self.err(
            Diagnostic::error(format!("expected `:` {after}, found {found}"), self.span())
                .with_help("a line that opens a block ends with `:`"),
        ))
    }

    /// After the `:` of a block header: `NEWLINE INDENT`.
    fn expect_block_start(&mut self) -> PResult<()> {
        if !self.at(&TokenKind::Newline) {
            let found = self.describe_current();
            return Err(self.err(
                Diagnostic::error(
                    format!("expected a newline after `:`, found {found}"),
                    self.span(),
                )
                .with_help("the block body goes on the following line(s), indented deeper"),
            ));
        }
        self.advance();
        if !self.at(&TokenKind::Indent) {
            let found = self.describe_current();
            return Err(self.err(
                Diagnostic::error(
                    format!("expected an indented block, found {found}"),
                    self.span(),
                )
                .with_help("indent the block body with spaces, e.g. 4"),
            ));
        }
        self.advance();
        Ok(())
    }

    /// `":" NEWLINE INDENT stmt+ DEDENT`.
    fn parse_block(&mut self, ctx: Ctx, colon_after: &str) -> PResult<Block> {
        self.expect_colon(colon_after)?;
        self.expect_block_start()?;
        let block = self.parse_stmts(ctx);
        self.expect(&TokenKind::Dedent, "end of block")?;
        Ok(block)
    }

    /// `":" NEWLINE INDENT prop* stmt+ DEDENT`.
    fn parse_prop_body(
        &mut self,
        ctx: Ctx,
        known: Option<&[&str]>,
        what: &str,
        colon_after: &str,
    ) -> PResult<(Vec<Property>, Block)> {
        self.expect_colon(colon_after)?;
        self.expect_block_start()?;
        let mut props = Vec::new();
        while self.at_prop_line() {
            let before = self.pos;
            match self.parse_prop(known, what, &props) {
                Ok(p) => props.push(p),
                Err(Fail) => self.sync(),
            }
            if self.pos == before {
                self.advance();
            }
        }
        let block = self.parse_stmts(ctx);
        self.expect(&TokenKind::Dedent, "end of block")?;
        Ok((props, block))
    }

    /// Statements until `DEDENT` / `EOF` (not consumed), with per-line
    /// recovery.
    fn parse_stmts(&mut self, ctx: Ctx) -> Block {
        let start = self.span().start;
        let mut stmts = Vec::new();
        while !matches!(self.peek(), TokenKind::Dedent | TokenKind::Eof) {
            let before = self.pos;
            match self.parse_stmt(ctx) {
                Ok(s) => stmts.push(s),
                Err(Fail) => self.sync(),
            }
            if self.pos == before {
                self.advance();
            }
        }
        let span = match (stmts.first(), stmts.last()) {
            (Some(f), Some(l)) => f.span.merge(l.span),
            _ => Span::at(start),
        };
        Block { stmts, span }
    }

    /// Property lines only (config bodies), until `DEDENT`.
    fn parse_prop_lines(&mut self, known: Option<&[&str]>, what: &str) -> PResult<Vec<Property>> {
        let mut props = Vec::new();
        while !matches!(self.peek(), TokenKind::Dedent | TokenKind::Eof) {
            let before = self.pos;
            if self.at_prop_line() {
                match self.parse_prop(known, what, &props) {
                    Ok(p) => props.push(p),
                    Err(Fail) => self.sync(),
                }
            } else {
                let found = self.describe_current();
                self.err(
                    Diagnostic::error(
                        format!("expected a `key: value` line, found {found}"),
                        self.span(),
                    )
                    .with_help(format!(
                        "a `browser:` block holds only {what}s such as `headless: true`"
                    )),
                );
                self.sync();
            }
            if self.pos == before {
                self.advance();
            }
        }
        Ok(props)
    }

    /// `IDENT ":" expr NEWLINE` or `IDENT ":" NEWLINE INDENT prop+ DEDENT`
    /// (nested map). Unknown / duplicate keys are reported but still
    /// returned so parsing continues.
    fn parse_prop(
        &mut self,
        known: Option<&[&str]>,
        what: &str,
        existing: &[Property],
    ) -> PResult<Property> {
        let key = self.ident("a property name")?;
        self.expect(&TokenKind::Colon, "`:`")?;
        let value = if self.at(&TokenKind::Newline) {
            self.expect_block_start()?;
            let nested = self.parse_prop_lines(None, what)?;
            self.expect(&TokenKind::Dedent, "end of block")?;
            let span = match (nested.first(), nested.last()) {
                (Some(f), Some(l)) => f.span.merge(l.span),
                _ => key.span,
            };
            let entries = nested
                .into_iter()
                .map(|p| (MapKey::Ident(p.key), p.value))
                .collect();
            Expr {
                kind: ExprKind::Map(entries),
                span,
            }
        } else {
            let value = self.parse_expr()?;
            self.end_stmt(None)?;
            value
        };
        if let Some(known) = known {
            if !known.contains(&key.name.as_str()) {
                let help = did_you_mean(&key.name, known.iter().copied())
                    .unwrap_or_else(|| format!("known {what}s: {}", known.join(", ")));
                self.err(
                    Diagnostic::error(format!("unknown {what} `{}`", key.name), key.span)
                        .with_help(help),
                );
            }
        }
        if let Some(prev) = existing.iter().find(|p| p.key.name == key.name) {
            self.err(
                Diagnostic::error(format!("duplicate {what} `{}`", key.name), key.span)
                    .with_secondary(prev.key.span, "first set here"),
            );
        }
        Ok(Property {
            span: key.span.merge(value.span),
            key,
            value,
        })
    }

    fn prop_line_error(&mut self, ctx: Ctx) -> Fail {
        let TokenKind::Ident(name) = self.peek().clone() else {
            return self.unexpected("a statement");
        };
        let span = self.span().merge(self.tokens[self.pos + 1].span);
        if ctx.takes_props() {
            return self.err(
                Diagnostic::error("property lines must come before statements", span)
                    .with_label(format!("`{name}:` is a property line"))
                    .with_help(format!(
                        "move `{name}: …` above the first statement of {}",
                        ctx.noun()
                    )),
            );
        }
        let help = if let Some(kw) = suggest(&name, KEYWORDS.iter().copied()) {
            format!("did you mean `{kw}`?")
        } else if ctx == Ctx::TopLevel && BROWSER_PROPS.contains(&name.as_str()) {
            format!("`{name}` is a browser option; put it inside a `browser:` block")
        } else {
            "`key: value` lines are only allowed in a `browser:` block and at the start of \
             task / actor / supervisor / `parallel for` bodies"
                .to_string()
        };
        self.err(
            Diagnostic::error(format!("unexpected property line `{name}:`"), span)
                .with_label("not a statement")
                .with_help(help),
        )
    }

    // ---- statements ----------------------------------------------------------

    fn parse_stmt(&mut self, ctx: Ctx) -> PResult<Stmt> {
        let start = self.span();
        if self.at_prop_line() {
            return Err(self.prop_line_error(ctx));
        }
        let kind = match self.peek().clone() {
            TokenKind::If => self.parse_if()?,
            TokenKind::For => self.parse_for()?,
            TokenKind::Parallel => self.parse_parallel_for(ctx)?,
            TokenKind::While => {
                self.advance();
                let cond = self.parse_expr()?;
                let body = self.parse_block(Ctx::Block, "after the `while` condition")?;
                StmtKind::While { cond, body }
            }
            TokenKind::Loop => {
                self.advance();
                let body = self.parse_block(Ctx::Block, "after `loop`")?;
                StmtKind::Loop { body }
            }
            TokenKind::Try => self.parse_try()?,
            TokenKind::Fn if !matches!(self.peek_n(1), TokenKind::LParen) => {
                StmtKind::Fn(self.parse_fn_decl()?)
            }
            TokenKind::On => {
                if ctx != Ctx::Actor {
                    return Err(self.err(
                        Diagnostic::error("`on …:` handlers are only allowed at the top level or directly inside an actor body", self.span())
                            .with_help("move this handler to the top level of the file"),
                    ));
                }
                StmtKind::Handler(self.parse_handler(ctx)?)
            }
            kw @ (TokenKind::Task | TokenKind::Actor | TokenKind::Supervisor) => {
                let word = kw.keyword_text().unwrap_or("declaration");
                return Err(self.err(
                    Diagnostic::error(
                        format!("`{word}` declarations are only allowed at the top level"),
                        self.span(),
                    )
                    .with_help(format!("move this `{word}` out of {}", ctx.noun())),
                ));
            }
            TokenKind::Return => {
                self.advance();
                let value = if self.at(&TokenKind::Newline) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.end_stmt(None)?;
                StmtKind::Return(value)
            }
            TokenKind::Break => {
                self.advance();
                self.end_stmt(None)?;
                StmtKind::Break
            }
            TokenKind::Continue => {
                self.advance();
                self.end_stmt(None)?;
                StmtKind::Continue
            }
            TokenKind::Emit => {
                self.advance();
                let value = self.parse_expr()?;
                self.end_stmt(None)?;
                StmtKind::Emit(value)
            }
            TokenKind::Exit => {
                self.advance();
                let code = if self.at(&TokenKind::LParen) {
                    self.advance();
                    let e = self.parse_expr()?;
                    self.expect(&TokenKind::RParen, "`)`")?;
                    Some(e)
                } else {
                    None
                };
                if !self.at(&TokenKind::Newline) && !self.prev_is_dedent() {
                    let text = self.current_text().to_string();
                    let found = self.describe_current();
                    return Err(self.err(
                        Diagnostic::error(
                            format!("expected end of line after `exit`, found {found}"),
                            self.span(),
                        )
                        .with_help(format!("did you mean `exit({text})`?")),
                    ));
                }
                self.end_stmt(None)?;
                StmtKind::Exit(code)
            }
            TokenKind::Elif | TokenKind::Else => {
                let word = self.peek().keyword_text().unwrap_or("else");
                return Err(self.err(
                    Diagnostic::error(format!("`{word}` without a matching `if`"), self.span())
                        .with_help(format!("`{word}` must directly follow an `if` or `elif` block at the same indentation")),
                ));
            }
            TokenKind::Catch => {
                return Err(self.err(
                    Diagnostic::error("`catch` without a matching `try`", self.span()).with_help(
                        "`catch` must directly follow a `try:` block at the same indentation",
                    ),
                ));
            }
            TokenKind::Newline | TokenKind::Indent | TokenKind::Dedent | TokenKind::Eof => {
                return Err(self.unexpected("a statement"));
            }
            _ => {
                let expr = self.parse_expr()?;
                let compound = compound_op(self.peek());
                if self.at(&TokenKind::Eq) || compound.is_some() {
                    if !matches!(
                        expr.kind,
                        ExprKind::Ident(_) | ExprKind::Field { .. } | ExprKind::Index { .. }
                    ) {
                        let op = self.peek().punct_text().unwrap_or("=");
                        return Err(self.err(
                            Diagnostic::error("invalid assignment target", expr.span)
                                .with_label("cannot assign to this expression")
                                .with_help(format!("assign to a name (`x {op} …`), a field (`m.k {op} …`) or an index (`xs[0] {op} …`)")),
                        ));
                    }
                    self.advance();
                    let value = self.parse_expr()?;
                    self.end_stmt(None)?;
                    match compound {
                        Some(op) => StmtKind::CompoundAssign {
                            target: expr,
                            op,
                            value,
                        },
                        None => StmtKind::Assign {
                            target: expr,
                            value,
                        },
                    }
                } else {
                    self.end_stmt(Some(&expr))?;
                    StmtKind::Expr(expr)
                }
            }
        };
        Ok(Stmt {
            kind,
            span: start.merge(Span::at(self.last_end)),
        })
    }

    /// Consume the statement terminator: `NEWLINE`, or nothing if a block
    /// (lambda body) just closed the line.
    fn end_stmt(&mut self, expr: Option<&Expr>) -> PResult<()> {
        if self.at(&TokenKind::Newline) {
            self.advance();
            return Ok(());
        }
        if self.prev_is_dedent() || self.at_eof() {
            return Ok(());
        }
        let found = self.describe_current();
        let mut d = Diagnostic::error(format!("expected end of line, found {found}"), self.span())
            .with_label("unexpected");
        if let Some(Expr {
            kind: ExprKind::Ident(id),
            ..
        }) = expr
        {
            if let Some(kw) = suggest(&id.name, KEYWORDS.iter().copied()) {
                d = d.with_help(format!("did you mean `{kw}`?"));
            } else if matches!(
                self.peek(),
                TokenKind::Int(_)
                    | TokenKind::Float(_)
                    | TokenKind::Str(_)
                    | TokenKind::Duration(_)
                    | TokenKind::Ident(_)
            ) {
                let text = self.current_text();
                d = d.with_help(format!("did you mean `{}({text})`?", id.name));
            }
        }
        if self.at(&TokenKind::Colon) {
            d = d.with_help("`:` only opens a block after `if`, `for`, `while`, `loop`, `try`, `fn`, `on` or a declaration");
        }
        Err(self.err(d))
    }

    fn parse_if(&mut self) -> PResult<StmtKind> {
        self.expect(&TokenKind::If, "`if`")?;
        let cond = self.parse_expr()?;
        let block = self.parse_block(Ctx::Block, "after the `if` condition")?;
        let mut branches = vec![(cond, block)];
        let mut else_block = None;
        loop {
            if self.at(&TokenKind::Elif) {
                self.advance();
                let cond = self.parse_expr()?;
                let block = self.parse_block(Ctx::Block, "after the `elif` condition")?;
                branches.push((cond, block));
            } else if self.at(&TokenKind::Else) {
                self.advance();
                if self.at(&TokenKind::If) {
                    return Err(self.err(
                        Diagnostic::error("`else if` is spelled `elif`", self.span())
                            .with_help("write `elif cond:`"),
                    ));
                }
                else_block = Some(self.parse_block(Ctx::Block, "after `else`")?);
                break;
            } else {
                break;
            }
        }
        Ok(StmtKind::If {
            branches,
            else_block,
        })
    }

    fn parse_for(&mut self) -> PResult<StmtKind> {
        self.expect(&TokenKind::For, "`for`")?;
        let var = self.ident("a loop variable")?;
        self.expect(&TokenKind::In, "`in`")?;
        let iter = self.parse_expr()?;
        let body = self.parse_block(Ctx::Block, "after the `for` header")?;
        Ok(StmtKind::For { var, iter, body })
    }

    fn parse_parallel_for(&mut self, ctx: Ctx) -> PResult<StmtKind> {
        self.expect(&TokenKind::Parallel, "`parallel`")?;
        if !self.at(&TokenKind::For) {
            return Err(self.unexpected("`for` after `parallel`"));
        }
        self.advance();
        let var = self.ident("a loop variable")?;
        self.expect(&TokenKind::In, "`in`")?;
        let iter = self.parse_expr()?;
        let body_ctx = if ctx == Ctx::Supervisor {
            Ctx::Supervisor
        } else {
            Ctx::ParallelFor
        };
        let (opts, body) = self.parse_prop_body(
            body_ctx,
            Some(PARALLEL_FOR_PROPS),
            "parallel for option",
            "after the `parallel for` header",
        )?;
        Ok(StmtKind::ParallelFor {
            var,
            iter,
            opts,
            body,
        })
    }

    fn parse_try(&mut self) -> PResult<StmtKind> {
        self.expect(&TokenKind::Try, "`try`")?;
        let body = self.parse_block(Ctx::Block, "after `try`")?;
        if !self.at(&TokenKind::Catch) {
            let found = self.describe_current();
            return Err(self.err(
                Diagnostic::error(
                    format!("expected `catch` after the `try` block, found {found}"),
                    self.span(),
                )
                .with_help(
                    "every `try:` needs a `catch e:` (or `catch:`) block at the same indentation",
                ),
            ));
        }
        self.advance();
        let catch_var = if self.at_ident() {
            Some(self.ident("an error binding")?)
        } else {
            None
        };
        let catch_block = self.parse_block(Ctx::Block, "after `catch`")?;
        Ok(StmtKind::Try {
            body,
            catch_var,
            catch_block,
        })
    }

    // ---- parameters and arguments ------------------------------------------

    fn parse_params(&mut self) -> PResult<Vec<Param>> {
        self.expect(&TokenKind::LParen, "`(` to start the parameter list")?;
        let mut params: Vec<Param> = Vec::new();
        while !self.at(&TokenKind::RParen) {
            let name = self.ident("a parameter name")?;
            let default = if self.eat(&TokenKind::Colon) {
                Some(self.parse_expr()?)
            } else {
                None
            };
            if let Some(prev) = params.iter().find(|p| p.name.name == name.name) {
                self.err(
                    Diagnostic::error(format!("duplicate parameter `{}`", name.name), name.span)
                        .with_secondary(prev.name.span, "first declared here"),
                );
            }
            let span = default
                .as_ref()
                .map_or(name.span, |d| name.span.merge(d.span));
            params.push(Param {
                name,
                default,
                span,
            });
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`,` or `)` in the parameter list")?;
        Ok(params)
    }

    /// `"(" (arg ("," arg)* ","?)? ")"` — the `(` is the current token.
    fn parse_call_args(&mut self) -> PResult<Vec<Arg>> {
        self.expect(&TokenKind::LParen, "`(`")?;
        let mut args: Vec<Arg> = Vec::new();
        let mut seen_keyword = false;
        while !self.at(&TokenKind::RParen) {
            let start = self.span();
            if self.at_prop_line() {
                let name = self.ident("an argument name")?;
                self.advance(); // ':'
                let value = self.parse_expr()?;
                if let Some(prev) = args
                    .iter()
                    .find(|a| a.name.as_ref().is_some_and(|n| n.name == name.name))
                {
                    let prev_span = prev.name.as_ref().map(|n| n.span).unwrap_or(prev.span);
                    self.err(
                        Diagnostic::error(
                            format!("duplicate keyword argument `{}`", name.name),
                            name.span,
                        )
                        .with_secondary(prev_span, "first passed here"),
                    );
                }
                seen_keyword = true;
                args.push(Arg {
                    span: start.merge(value.span),
                    name: Some(name),
                    value,
                });
            } else {
                let value = self.parse_expr()?;
                if seen_keyword {
                    self.err(
                        Diagnostic::error("positional argument after keyword argument", value.span)
                            .with_help("keyword arguments (`name: value`) must come after all positional arguments"),
                    );
                }
                args.push(Arg {
                    span: value.span,
                    name: None,
                    value,
                });
            }
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        self.expect(&TokenKind::RParen, "`,` or `)` in the argument list")?;
        Ok(args)
    }

    // ---- expressions --------------------------------------------------------

    /// Parse one expression.
    fn parse_expr(&mut self) -> PResult<Expr> {
        self.parse_bp(0)
    }

    fn parse_bp(&mut self, min_bp: u8) -> PResult<Expr> {
        let mut lhs = self.parse_prefix()?;
        if is_block_lambda(&lhs) {
            // The indented body closed the line; whatever follows is the
            // next statement, not an operand.
            return Ok(lhs);
        }
        while let Some((lbp, rbp, op)) = binary_op(self.peek()) {
            if lbp < min_bp {
                break;
            }
            self.advance();
            let rhs = self.parse_bp(rbp)?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                kind: match op {
                    BinKind::Op(op) => ExprKind::Binary {
                        op,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                    },
                    BinKind::Range(inclusive) => ExprKind::Range {
                        start: Box::new(lhs),
                        end: Box::new(rhs),
                        inclusive,
                    },
                },
                span,
            };
            // Non-associative levels: comparison and range.
            if let Some((_, _, next)) = binary_op(self.peek()) {
                if op.is_cmp() && next.is_cmp() {
                    return Err(self.err(
                        Diagnostic::error("comparison operators cannot be chained", self.span())
                            .with_label("second comparison")
                            .with_help("write `a < b and b < c`"),
                    ));
                }
                if op.is_range() && next.is_range() {
                    return Err(self.err(
                        Diagnostic::error("ranges cannot be chained", self.span())
                            .with_help("a range is `start..end` or `start..=end`"),
                    ));
                }
            }
        }
        Ok(lhs)
    }

    fn parse_prefix(&mut self) -> PResult<Expr> {
        match self.peek() {
            TokenKind::Not => {
                let start = self.advance().span;
                let expr = self.parse_bp(5)?;
                Ok(Expr {
                    span: start.merge(expr.span),
                    kind: ExprKind::Unary {
                        op: UnaryOp::Not,
                        expr: Box::new(expr),
                    },
                })
            }
            TokenKind::Minus => {
                let start = self.advance().span;
                let expr = self.parse_bp(15)?;
                let span = start.merge(expr.span);
                let kind = match expr.kind {
                    ExprKind::Int(v) => ExprKind::Int(-v),
                    ExprKind::Float(v) => ExprKind::Float(-v),
                    _ => ExprKind::Unary {
                        op: UnaryOp::Neg,
                        expr: Box::new(expr),
                    },
                };
                Ok(Expr { kind, span })
            }
            TokenKind::Spawn => {
                let start = self.advance().span;
                let target = self.parse_postfix()?;
                if !matches!(target.kind, ExprKind::Call { .. } | ExprKind::Method { .. }) {
                    return Err(self.err(
                        Diagnostic::error("`spawn` needs a call", target.span)
                            .with_label("not a call")
                            .with_help(
                                "write `spawn name(args)` — a fn, task or actor with its arguments",
                            ),
                    ));
                }
                Ok(Expr {
                    span: start.merge(target.span),
                    kind: ExprKind::Spawn(Box::new(target)),
                })
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.parse_primary()?;
        if is_block_lambda(&expr) {
            // `f = fn(x):` + block, then `[1, 2].each(f)` on the next line:
            // the `[` starts a statement, it does not index the lambda.
            return Ok(expr);
        }
        loop {
            match self.peek() {
                TokenKind::LParen => {
                    let args = self.parse_call_args()?;
                    expr = Expr {
                        span: expr.span.merge(Span::at(self.last_end)),
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                    };
                }
                TokenKind::Dot => {
                    self.advance();
                    let name = self.member_name(&expr)?;
                    if self.at(&TokenKind::LParen) {
                        let args = self.parse_call_args()?;
                        expr = Expr {
                            span: expr.span.merge(Span::at(self.last_end)),
                            kind: ExprKind::Method {
                                receiver: Box::new(expr),
                                name,
                                args,
                            },
                        };
                    } else {
                        expr = Expr {
                            span: expr.span.merge(name.span),
                            kind: ExprKind::Field {
                                receiver: Box::new(expr),
                                name,
                            },
                        };
                    }
                }
                TokenKind::LBracket => {
                    self.advance();
                    let index = self.parse_expr()?;
                    let end = self.expect(&TokenKind::RBracket, "`]`")?;
                    expr = Expr {
                        span: expr.span.merge(end),
                        kind: ExprKind::Index {
                            receiver: Box::new(expr),
                            index: Box::new(index),
                        },
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// The name after a `.`, with hints for `page.1` / `xs.0`. Keywords
    /// are accepted as member names (`event.continue()`).
    fn member_name(&mut self, receiver: &Expr) -> PResult<Ident> {
        if let Some(kw) = self.peek().keyword_text() {
            let name = kw.to_string();
            let span = self.advance().span;
            return Ok(Ident { name, span });
        }
        if let TokenKind::Int(n) = self.peek() {
            let n = *n;
            let help = match &receiver.kind {
                ExprKind::Ident(id) if id.name == "page" => format!("did you mean `page({n})`?"),
                _ => format!("use `[{n}]` to index a list"),
            };
            return Err(self
                .err(Diagnostic::error("expected a name after `.`", self.span()).with_help(help)));
        }
        self.ident("a name after `.`")
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let span = self.span();
        let kind = match self.peek().clone() {
            TokenKind::Int(v) => {
                self.advance();
                ExprKind::Int(v)
            }
            TokenKind::Float(v) => {
                self.advance();
                ExprKind::Float(v)
            }
            TokenKind::Duration(d) => {
                self.advance();
                ExprKind::Duration(d)
            }
            TokenKind::True => {
                self.advance();
                ExprKind::Bool(true)
            }
            TokenKind::False => {
                self.advance();
                ExprKind::Bool(false)
            }
            TokenKind::Nil => {
                self.advance();
                ExprKind::Nil
            }
            TokenKind::Str(parts) => {
                self.advance();
                ExprKind::Str(self.parse_str_parts(parts)?)
            }
            TokenKind::Ident(name) => {
                self.advance();
                ExprKind::Ident(Ident { name, span })
            }
            TokenKind::LParen => {
                self.advance();
                let inner = self.parse_expr()?;
                let end = self.expect(&TokenKind::RParen, "`)`")?;
                return Ok(Expr {
                    kind: inner.kind,
                    span: span.merge(end),
                });
            }
            TokenKind::LBracket => return self.parse_list(),
            TokenKind::LBrace => return self.parse_map(),
            TokenKind::Fn => return self.parse_lambda(),
            TokenKind::Eq => {
                return Err(self.err(
                    Diagnostic::error("expected an expression, found `=`", span)
                        .with_help("`==` compares; `=` assigns and only appears as `name = value`"),
                ));
            }
            _ => return Err(self.unexpected("an expression")),
        };
        Ok(Expr { kind, span })
    }

    fn parse_str_parts(&mut self, parts: Vec<StrPart>) -> PResult<Vec<StrSegment>> {
        let mut segments = Vec::with_capacity(parts.len());
        let mut failed = false;
        for part in parts {
            match part {
                StrPart::Lit(s) => segments.push(StrSegment::Lit(s)),
                StrPart::Expr(_, span) => match self.parse_interpolation(span) {
                    Ok(e) => segments.push(StrSegment::Expr(e)),
                    Err(Fail) => failed = true,
                },
            }
        }
        if failed {
            Err(Fail)
        } else {
            Ok(segments)
        }
    }

    /// Lex and parse the `{expr}` source at `span` with a sub-parser.
    fn parse_interpolation(&mut self, span: Span) -> PResult<Expr> {
        let tokens = match lexer::lex_expr(self.name, self.source, span) {
            Ok(t) => t,
            Err(d) => {
                self.diags.extend(d);
                return Err(Fail);
            }
        };
        let mut sub = Parser::new(self.name, self.source, tokens);
        sub.in_interp = true;
        let result = sub.parse_expr().and_then(|e| {
            if sub.at_eof() {
                Ok(e)
            } else {
                let found = sub.describe_current();
                Err(sub.err(
                    Diagnostic::error(
                        format!("expected `}}` to end the interpolation, found {found}"),
                        sub.span(),
                    )
                    .with_help("write `\\{` for a literal brace"),
                ))
            }
        });
        self.diags.extend(sub.diags);
        result
    }

    fn parse_list(&mut self) -> PResult<Expr> {
        let start = self.expect(&TokenKind::LBracket, "`[`")?;
        let mut items = Vec::new();
        while !self.at(&TokenKind::RBracket) {
            items.push(self.parse_expr()?);
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let end = self.expect(&TokenKind::RBracket, "`,` or `]` in the list")?;
        Ok(Expr {
            kind: ExprKind::List(items),
            span: start.merge(end),
        })
    }

    fn parse_map(&mut self) -> PResult<Expr> {
        let start = self.expect(&TokenKind::LBrace, "`{`")?;
        let mut entries = Vec::new();
        while !self.at(&TokenKind::RBrace) {
            let key_span = self.span();
            let key = match self.peek().clone() {
                TokenKind::Ident(name) => {
                    self.advance();
                    MapKey::Ident(Ident {
                        name,
                        span: key_span,
                    })
                }
                TokenKind::Str(parts) => {
                    self.advance();
                    MapKey::Str(self.parse_str_parts(parts)?, key_span)
                }
                TokenKind::LBracket => {
                    self.advance();
                    let e = self.parse_expr()?;
                    self.expect(&TokenKind::RBracket, "`]` after the computed key")?;
                    MapKey::Expr(e)
                }
                kw if kw.is_keyword() => {
                    let word = kw.keyword_text().unwrap_or_default();
                    return Err(self.err(
                        Diagnostic::error(
                            format!("`{word}` is a reserved keyword and cannot be a bare map key"),
                            key_span,
                        )
                        .with_help(format!("quote it: `{{\"{word}\": …}}`")),
                    ));
                }
                _ => return Err(self.unexpected("a map key (`name`, `\"string\"` or `[expr]`)")),
            };
            self.expect(&TokenKind::Colon, "`:` after the map key")?;
            let value = self.parse_expr()?;
            entries.push((key, value));
            if !self.eat(&TokenKind::Comma) {
                break;
            }
        }
        let end = self.expect(&TokenKind::RBrace, "`,` or `}` in the map")?;
        Ok(Expr {
            kind: ExprKind::Map(entries),
            span: start.merge(end),
        })
    }

    /// `fn (params) : (expr | block)` — the `fn` is the current token.
    fn parse_lambda(&mut self) -> PResult<Expr> {
        let start = self.expect(&TokenKind::Fn, "`fn`")?;
        if let TokenKind::Ident(name) = self.peek().clone() {
            return Err(self.err(
                Diagnostic::error("a named `fn` is a declaration, not an expression", self.span())
                    .with_help(format!("declare it on its own line as `fn {name}(…):`, or drop the name for a lambda: `fn(…): …`")),
            ));
        }
        let params = self.parse_params()?;
        self.expect_colon("after the lambda parameters")?;
        let body = if self.at(&TokenKind::Newline) {
            self.expect_block_start()?;
            let block = self.parse_stmts(Ctx::Block);
            self.expect(&TokenKind::Dedent, "end of block")?;
            LambdaBody::Block(block)
        } else {
            LambdaBody::Expr(Box::new(self.parse_expr()?))
        };
        Ok(Expr {
            span: start.merge(Span::at(self.last_end)),
            kind: ExprKind::Lambda { params, body },
        })
    }

    /// An identifier; keywords get a dedicated message.
    fn ident(&mut self, what: &str) -> PResult<Ident> {
        match self.peek().clone() {
            TokenKind::Ident(name) => {
                let span = self.advance().span;
                Ok(Ident { name, span })
            }
            kw if kw.is_keyword() => {
                let word = kw.keyword_text().unwrap_or_default();
                Err(self.err(
                    Diagnostic::error(
                        format!("`{word}` is a reserved keyword and cannot be used as {what}"),
                        self.span(),
                    )
                    .with_help("pick another name"),
                ))
            }
            _ => Err(self.unexpected(what)),
        }
    }
}

/// A lambda whose body is an indented block: it ends the logical line, so
/// no postfix or infix operator can follow it.
fn is_block_lambda(e: &Expr) -> bool {
    matches!(
        &e.kind,
        ExprKind::Lambda {
            body: LambdaBody::Block(_),
            ..
        }
    )
}

/// The operator a compound assignment token applies (`+=` → `Add`).
fn compound_op(kind: &TokenKind) -> Option<BinaryOp> {
    Some(match kind {
        TokenKind::PlusEq => BinaryOp::Add,
        TokenKind::MinusEq => BinaryOp::Sub,
        TokenKind::StarEq => BinaryOp::Mul,
        TokenKind::SlashEq => BinaryOp::Div,
        TokenKind::PercentEq => BinaryOp::Rem,
        _ => return None,
    })
}

#[derive(Debug, Clone, Copy)]
enum BinKind {
    Op(BinaryOp),
    Range(bool),
}

impl BinKind {
    fn is_cmp(self) -> bool {
        matches!(
            self,
            BinKind::Op(
                BinaryOp::Eq
                    | BinaryOp::NotEq
                    | BinaryOp::Lt
                    | BinaryOp::LtEq
                    | BinaryOp::Gt
                    | BinaryOp::GtEq
            )
        )
    }

    fn is_range(self) -> bool {
        matches!(self, BinKind::Range(_))
    }
}

/// `(left bp, right bp, operator)` for an infix token.
fn binary_op(kind: &TokenKind) -> Option<(u8, u8, BinKind)> {
    use BinaryOp as B;
    Some(match kind {
        TokenKind::Or => (1, 2, BinKind::Op(B::Or)),
        TokenKind::And => (3, 4, BinKind::Op(B::And)),
        TokenKind::EqEq => (7, 8, BinKind::Op(B::Eq)),
        TokenKind::NotEq => (7, 8, BinKind::Op(B::NotEq)),
        TokenKind::Lt => (7, 8, BinKind::Op(B::Lt)),
        TokenKind::LtEq => (7, 8, BinKind::Op(B::LtEq)),
        TokenKind::Gt => (7, 8, BinKind::Op(B::Gt)),
        TokenKind::GtEq => (7, 8, BinKind::Op(B::GtEq)),
        TokenKind::DotDot => (9, 10, BinKind::Range(false)),
        TokenKind::DotDotEq => (9, 10, BinKind::Range(true)),
        TokenKind::Plus => (11, 12, BinKind::Op(B::Add)),
        TokenKind::Minus => (11, 12, BinKind::Op(B::Sub)),
        TokenKind::Star => (13, 14, BinKind::Op(B::Mul)),
        TokenKind::Slash => (13, 14, BinKind::Op(B::Div)),
        TokenKind::Percent => (13, 14, BinKind::Op(B::Rem)),
        _ => return None,
    })
}
