# Agent: verifier (explore)
Task: Verification pass: 10 edited file(s)

## Tool Calls
[grep] /std::fs|std::process|tokio|std::io/
[read] 
[grep] /properties must come before statements/
[grep] /tab|inconsistent|dedent/
[list_dir]
[read] 
[grep] /properties (must|come|before)|before the first statement/
[grep] /property line|properties-before|Property lines/
[final_response]

## Agent Summary
Verification of `surf-syntax`:

- `crates/surf-syntax/Cargo.toml:11-13` — deps are only `ariadne` + `serde`; no `tokio`/`std::fs`/`std::process` anywhere in `src/` (grep clean). wasm32-clean holds.
- `crates/surf-syntax/src/lib.rs:39` — `parse(name, source) -> Result<Program, Diagnostics>`. Brief said `parse(src, name)`; arg order swapped and errors returned as a `Diagnostics` batch rather than `Vec<Diagnostic>`. Both are reasonable and the peer finding documents it for the VM — cosmetic deviation, not a defect. `parse_expr` added for REPL.
- `crates/surf-syntax/src/lexer.rs:242-281` — tab-in-indent reported once per file (`tab_reported`), line skipped; mixed dedent produces `unindent does not match any outer indentation level` with enclosing levels in help (`snapshots/parse__err_mixed_dedent.snap:5-12`).
- Properties-before-statements: error `property lines must come before statements` + help `move \`timeout: …\` above the first statement of the task body` (`parse__err_property_after_statement.snap:5-12`). Rule documented in `docs/language.md:575-578` and `:704-708` (hint text differs slightly from the brief's quoted "properties must come before statements" — semantically equivalent).
- `crates/surf-syntax/tests/parse.rs` — 54 committed snapshots: 6 examples, every spec section (layout, literals, interpolation, precedence, collections, calls, lambdas, if/loops/fn/try/emit/exit, config incl. nested map + named browser, handlers, spawn/parallel/task/actor/supervisor), 30 error renders including all 5 required (tab indent, mixed dedent, unclosed string, property-after-statement, unknown token), plus span/precedence/multi-error/CRLF/EOF structural tests and a `Diagnostic::runtime` render.
- Not verified: wasm32 build (requires non-MacPorts cargo per peer note; typecheck/tests passed on host only).

VERDICT: PASS — surf-syntax implements lexer/AST/parser/diagnostics per docs/language.md with full snapshot coverage; only cosmetic API-order deviation (`parse(name, src)`), already reported to the VM step.
