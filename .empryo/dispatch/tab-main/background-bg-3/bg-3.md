# Agent: bg-3 (code)
Task: # SURF — shared brief for every node (read fully before acting)

Repo: /Users/chaser/surf. Host: macOS arm64, cargo 1.97 stable, Google Chrome at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`, Playwright Chromium builds under ~/Library/Caches/ms-playwright, Apostate (a Chromium fork

## Tool Calls
[read] 
[read] 
[read] 
[shell]
[read] 
[grep] /ast::|StrPart|ExprKind|StmtKind|Item::/
[shell]
[shell]
[read] 
[multi_edit] crates/surf-syntax/src/token.rs
[multi_edit] crates/surf-syntax/src/span.rs
[edit_file] crates/surf-syntax/src/diagnostics.rs
[edit_file] crates/surf-syntax/src/lexer.rs
[multi_edit] crates/surf-syntax/src/lexer.rs
[multi_edit] crates/surf-syntax/src/ast.rs
[multi_edit] crates/surf-syntax/src/ast.rs
[edit_file] crates/surf-syntax/src/parser.rs
[shell]
[edit_file] crates/surf-syntax/src/lib.rs
[shell]
[shell]
[edit_file] crates/surf-syntax/tests/parse.rs
[shell]
[shell]
[read] 
[multi_edit] crates/surf-syntax/src/lexer.rs
[multi_edit] crates/surf-syntax/tests/parse.rs
[shell]
[shell]
[edit_file] crates/surf-syntax/tests/parse.rs
[shell]
[shell]
[multi_edit] crates/surf-syntax/src/diagnostics.rs
[read] 
[multi_edit] crates/surf-syntax/src/lexer.rs
[grep] /expected `}` before here/
[multi_edit] crates/surf-syntax/src/lexer.rs
[grep] /fn unexpected|let mut d = Diagnostic::error\(format!\("expected end of line|comparison operators cannot be chained|TokenKind::Fn if matches!\(self.peek_n\(1\), TokenKind::Ident\(_\)\)|let name = self.ident\("a (task|function|an actor|supervisor) name"\)|let name = self.ident\("an actor name"\)|let name = self.ident\("a supervisor name"\)/
[read] 
[multi_edit] crates/surf-syntax/src/parser.rs
[multi_edit] crates/surf-syntax/tests/parse.rs
[shell]
[shell]
[shell]
[shell]
[multi_edit] crates/surf-syntax/src/parser.rs
[shell]
[shell]
[shell]
[shell]
[shell]
[shell]
[shell]
[shell]
[shell]
[multi_edit] docs/language.md
[shell]
[shell]
[edit_file] docs/language.md
[shell]
[shell]
[shell]
[shell]
[shell]
[shell]
[shell]
[shell]
[shell]
[shell]
[edit_file] TASKS.md
[shell]
[shell]

## Findings
**surf-syntax done — AST shape for the VM compiler:**
`surf_syntax::parse(name, src) -> Result<ast::Program, Diagnostics>` is implemented (commits d36c01c..ded6e91). Notes for surf-vm (step 6):
- AST derives Serialize/Deserialize. `ExprKind::Str(Vec<StrSegment>)` where `StrSegment::{Lit(String), Expr(Expr)}` — interpolations are already parsed (token::StrPart is lexer-only). Empty string = empty vec.
- `spawn` is an EXPRESSION: `ExprKind::Spawn(Box<Expr>)` (operand always Call/Method). There is no `StmtKind::Spawn`; a bare `spawn X()` line is `StmtKind::Expr(Spawn(..))`. Supervisor bodies are validated by the parser (only spawn lines / parallel-for of spawn lines).
- `-7` folds to `Int(-7)`; `(e)` keeps the inner kind with the widened span.
- Nested config maps: `proxy:` + indented `k: v` lines → `Property.value = ExprKind::Map`. Bare idents in config values (`cdp: pipe`) are `ExprKind::Ident` — treat unknown idents as symbols in config context.
- Parser already rejects unknown/duplicate keys for browser/task/actor/supervisor/parallel-for props and unknown handler events (lists: `parser::BROWSER_PROPS`, `TASK_PROPS`, `SUPERVISOR_PROPS`, `PARALLEL_FOR_PROPS`, `HANDLER_EVENTS`).
- `Diagnostic::runtime(msg, span, selector, cdp_method)` + `Diagnostic::render(name, src, color)` give the CLI error shape; `Diagnostics::render` renders a batch. `Span::line_col(src)` / `Span::slice(src)` helpers exist. `Diagnostics::unimplemented` was removed.
- `surf_syntax::parse_expr(name, src)` parses a single expression (REPL).
- Build note: the default `cargo` on this Mac is MacPorts' (no wasm target); use `RUSTC_WRAPPER= RUSTC=~/.cargo/bin/rustc ~/.cargo/bin/cargo build --target wasm32-unknown-unknown`.

## Agent Summary
`surf-syntax` implemented in 5 commits (d36c01c…ded6e91), scoped to `crates/surf-syntax/`, `docs/language.md`, `TASKS.md`.

**Built**
- `lexer.rs` — spaces-only INDENT/DEDENT (tab → diagnostic, inconsistent dedent → diagnostic listing the enclosing levels, `:` header without a deeper line → diagnostic), newline suppression inside brackets with a bracket stack (mismatch/unclosed diagnostics), ints with `_`, floats incl. exponents, durations (`1.5s`, overflow-checked), strings with the spec escapes + `\u{…}` and `{expr}` parts found by brace/nested-string scanning; `lex_expr` re-lexes an interpolation range with file-relative spans. Errors are collected, not first-only.
- `parser.rs` — recursive descent + Pratt (`or < and < not < cmp (non-assoc) < range (non-assoc) < + - < * / % < unary - < postfix`), statement-level recovery (sync to next logical line, skipping opened blocks), property-line rule exactly as specified (`property lines must come before statements` / top-level `headless: true` → "put it inside a `browser:` block"), nested-map props, known-key validation with edit-distance suggestions, supervisor body validation, `did you mean page(1)?` / `click("#a")` / `while` hints, `else if` → `elif`, chained comparison error.
- `diagnostics.rs` — `Diagnostic{severity,message,span,label,secondary,help,note}`, ariadne rendering (byte index type, lowercase `error:`), `Diagnostic::runtime(msg, span, selector, cdp_method)` for the VM/CLI, `edit_distance`/`suggest`.
- `ast.rs` — serde derives; `StrSegment` (parsed interpolation); `ExprKind::Spawn` (see spec change).
- Tests: 9 lexer unit tests, 62 integration tests — 26 YAML AST snapshots (every § of language.md + 6 examples), 29 error-render snapshots, span/precedence/recovery assertions. `surf check` on examples now reaches the (stub) compiler; on a bad file prints the rendered errors with exit 2.

**Spec resolutions (docs/language.md updated)**: `h = spawn crawl(…)` contradicted "spawn is a statement" → `spawn` is now a prefix expression over a postfix chain; `IDENT:` + indented block = nested map; actors take the task property set; handlers only at top level / directly in actors with a fixed event list; `if x: stmt` on one line is an error; lone `}` in strings is literal, `{}` is an error.

**Measured**: release parse of all 6 examples ≈ 5.9 µs/file, 46 MB/s (`cargo test -p surf-syntax --release -- --ignored perf`).

**Verification**: `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings` clean; `cargo test` green per crate (syntax 71, cdp 30, browser 4, runtime 1, cli 2); wasm32 build green — but only via rustup's toolchain: the `cargo` first on PATH is MacPorts' (no wasm target), so use `RUSTC_WRAPPER= RUSTC=~/.cargo/bin/rustc ~/.cargo/bin/cargo build -p surf-syntax --target wasm32-unknown-unknown`. One `cargo test --workspace` run hung >15 min while the surf-cdp agent was building concurrently; per-crate runs all pass.

**Deferred (TASKS.md)**: block lambdas inside brackets; raw-JS form for `eval(fn(): …)` bodies Surf can't parse; known-key lists duplicated between parser and spec.
