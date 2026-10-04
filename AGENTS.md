# Working agreement

For humans and agents working in this repo. Read `docs/architecture.md`
(crate map, contracts) and `docs/language.md` (spec) first; `DECISIONS.md`
lists what is settled.

## Crate map

| crate | owns | must not |
|-------|------|----------|
| `surf-syntax` | lexer, parser, AST, diagnostics | depend on tokio / `std::process` / `std::fs` (wasm32-clean) |
| `surf-vm` | bytecode compiler, async VM, `Value`, `Host` trait, stdlib | do IO; depend on tokio (wasm32-clean; `futures` only) |
| `surf-cdp` | transports, framing, connection/session mux, events, typed commands | know about pages, selectors, policy |
| `surf-browser` | discovery, launch flags, Xvfb, Browser/Page/World, auto-wait actions, input, network hooks, cookies | be `Send`; expose CDP method names above it |
| `surf-runtime` | `Host` impl, implicit pages, bare actions, handlers, tasks/actors/supervisors, lifetime | talk CDP directly |
| `surf-cli` | `surf run/check/doctor/repl`, exit codes | contain logic that belongs in runtime |

Dependency direction is strict: `cli → runtime → browser → cdp`, `runtime → vm → syntax`.

## Quiet-CDP rules (hard — judges grep for violations)

1. **Never** send `Runtime.enable`. Execution contexts come from
   `Page.createIsolatedWorld{frameId, worldName, grantUniveralAccess:true}`;
   evaluate with `Runtime.callFunctionOn{executionContextId}` /
   `Runtime.evaluate{contextId}`. World → runtime channel is
   `Runtime.addBinding{executionContextId}` + `Runtime.bindingCalled`.
2. **Never** pass `--enable-automation`,
   `--disable-blink-features=AutomationControlled`, or any `--disable-*`
   the user did not write. The full flag list lives in
   `surf_browser::launch::LaunchOptions::args` and nowhere else.
   `--user-data-dir` is always passed (Chrome ≥ 136 ignores pipe/port on
   the default profile). `--headless` only when headless.
3. `Page.enable` (ref-counted) is the only domain on by default.
   `Network` / `Fetch` only while a hook / intercept / proxy-auth needs
   them, disabled when the last one goes. **Never** `DOM.enable` —
   `DOM.getContentQuads`, `DOM.scrollIntoViewIfNeeded`, `Input.*`,
   `Page.captureScreenshot`, `Storage.*`, `Emulation.*` work without it.
4. Pipe transport by default (fd 3 / fd 4, `\0`-terminated JSON). No
   listening port unless the script says `cdp: 9222`.
5. Scripts never inject into the main world. Helpers live in the isolated
   world under random names.

There is no `stealth:` flag and no "stealth" vocabulary. Quiet is the only
mode.

## Building and testing

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p surf-syntax -p surf-vm --target wasm32-unknown-unknown   # must stay green
```

Tests that need Chrome use `surf_browser::discovery::chrome_or_skip(name)`:
`SURF_CHROME=/path/to/chrome` overrides discovery; otherwise `PATH` and the
platform defaults (`/Applications/Google Chrome.app/Contents/MacOS/Google
Chrome`, `/usr/bin/google-chrome`, …) are tried; when nothing is found the
test prints `skipping <name>: no Chrome found` and passes. They **must**
actually run on your machine — do not merge on a skip. The e2e suite is
`cargo test -p surf-cli --test e2e`.

Apostate (a Chromium fork under `~/Library/Caches/apostate`) is **not** a
dependency. Never touch `~/apostate`. Surf must work with stock Chrome.

## Definition of done

fmt, clippy (`-D warnings`), tests green; Chrome-needing tests ran for real;
docs updated in the same commit as the behaviour (`docs/language.md` for
language changes, `docs/architecture.md` for contract changes, `DECISIONS.md`
for new decisions); deferred work added to `TASKS.md`; report what you
built, what you measured, what you deferred.

## Commits

Small conventional commits: `feat(cdp): pipe transport`, `fix(vm): range
end`, `docs(language): clarify props`, `test(e2e): bindingCalled without
Runtime.enable`. No attribution trailers (no `Co-authored-by`, no
`Generated-by`). Never commit secrets, profiles, or `.empryo/` state.

## Style

- `#![forbid(unsafe_code)]` in every crate; if a transport truly needs
  `unsafe` (fd handling), isolate it in one module with `#![allow]` and a
  `// SAFETY:` comment.
- `#![warn(missing_docs)]` on library crates; every `pub` item has a doc
  line.
- Errors: `thiserror` enums in library crates; `surf_vm::RuntimeError`
  carries `span`, `selector`, `cdp_method`.
- Keep public contract signatures from `docs/architecture.md`; change them
  only together with the doc and every implementer.
