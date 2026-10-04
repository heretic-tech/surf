# Agent: bg-1 (code)
Task: # SURF — shared brief for every node (read fully before acting)

Repo: /Users/chaser/surf. Host: macOS arm64, cargo 1.97 stable, Google Chrome at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`, Playwright Chromium builds under ~/Library/Caches/ms-playwright, Apostate (a Chromium fork

## Tool Calls
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

## Agent Summary
Scaffold complete, pushed to **https://github.com/0xchasercat/surf** (private).

## Verification
- `cargo build --workspace`, `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`: clean.
- `cargo test --workspace`: 8 passed (pipe framing round-trip, quiet-flag assertions, selector prefixes, URL globs, page registry, e2e discovery + examples-present). `chrome_is_discoverable` ran for real against `/Applications/Google Chrome.app/…`.
- `cargo build -p surf-syntax -p surf-vm --target wasm32-unknown-unknown`: passes with the rustup toolchain. Caveat: `/opt/local/bin/cargo` (MacPorts 1.97) shadows `~/.cargo/bin` and lacks wasm std; the working invocation is recorded in TASKS.md.
- `surf doctor` finds Chrome; `surf check examples/hello.surf` exits 2 with an ariadne-rendered "lexer not implemented yet".

## Files
- `Cargo.toml` (workspace, pinned deps verified via `cargo search`; libc pinned to `0.2.190`, not the `1.0.0-alpha`), `rust-toolchain.toml`, `.gitignore`, `LICENSE`, `Cargo.lock`
- `crates/surf-syntax` (span, token, lexer, ast, parser, diagnostics — full AST defined), `surf-vm` (value, host, bytecode, compiler, vm, stdlib, cancel, error — contracts verbatim plus `Prop {Const, Lazy}` for lazy task props and `Vm::fork`), `surf-cdp` (transport/{pipe,ws,framing}, connection, session, event, error, protocol/{mod,generated} — `FORBIDDEN_METHODS` debug-asserted in `call_raw`, `enable_domain` rejects Runtime/DOM), `surf-browser` (discovery [real], launch [real `args()` + tests], page/Backing/Migration/rebind, world, selector, actions, input, network, cookies, observer, xvfb, browser), `surf-runtime` (host, config, pages, builtins, objects, handlers, tasks, actors, supervisors, lifetime), `surf-cli` (main, commands, tests/e2e.rs)
- `docs/architecture.md`, `docs/language.md` (full grammar + EBNF), `DECISIONS.md`, `TASKS.md`, `README.md`, `AGENTS.md`
- `protocol/{browser_protocol,js_protocol}.json` + `VERSION` (commit `d209a9a3`, r1710668, 2026-10-03)
- `.github/workflows/ci.yml` (fmt, clippy, wasm, test ubuntu+macos w/ setup-chrome+xvfb, e2e job)
- `examples/{hello,login,two-tabs,scrape-emit,parallel-pool,supervised}.surf`

## Spec decisions made while writing language.md
`parallel for` options are leading `key: value` lines in its body (same rule as task props); handler payload binds to `event`; supervisors auto-start after declarations; `fail("msg")` raises; `eval(fn(): …)` body is sent as raw JS.

## Deviations
- `gh repo create heretic-tech/surf` failed (no CreateRepository permission on the org) → created under `0xchasercat/surf`.
- Added `.empryo/orchestrations/runs/`, `desktop/`, `background/` to `.gitignore` (second commit) — they change on every agent step and would dirty every commit. `.empryo/plans/plan.md`, `orchestrations/surf-v01.json`, `config.json` are committed per the brief's list.
