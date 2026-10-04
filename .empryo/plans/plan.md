# Surf language & runtime v0.1

_depth: light — executor keeps current context_

## Context

Surf: a small indentation-based scripting language + Rust runtime for driving Chromium over raw CDP with zero boilerplate (implicit browser/page), green-thread concurrency with no async keywords, and "quiet by default" transport (pipe fd3/4, no automation flags, no Runtime.enable, isolated-world eval). Repo is empty (/Users/chaser/surf has only .empryo). Toolchain present: cargo 1.97, Google Chrome.app, playwright chromium-1243, apostate 155.0.8059.31 cache.

Research-backed facts the design rests on:
- Chrome pipe transport: `--remote-debugging-pipe` reads fd 3 / writes fd 4, messages are JSON terminated by \0 (`--remote-debugging-pipe=cbor` gives length-prefixed CBOR). Windows: `--remote-debugging-io-pipes=<readHandle>,<writeHandle>` (ChromeDriver's route) — pipe works on all 3 OSes, no loopback port ever.
- Chrome >=136 ignores pipe/port on the default user-data-dir → Surf always passes `--user-data-dir` (temp, wiped at exit, or `profile:`).
- `navigator.webdriver` + infobar come only from `--enable-automation`; never pass it. No `--disable-blink-features=AutomationControlled` needed.
- The real CDP tell used by Cloudflare/DataDome is `Runtime.enable` (console-serialisation getters / stack-trace capture). Fix = never send it: get execution contexts from `Page.createIsolatedWorld` (returns executionContextId), evaluate with `Runtime.evaluate{contextId}` / `Runtime.callFunctionOn`. Apostate measured FPJS `developer_tools=false` with this approach (Patchright) vs true under Playwright.
- Per-page proxy = `Target.createBrowserContext{proxyServer, proxyBypassList}`; authenticated proxies via `Fetch.enable{handleAuthRequests}` + `Fetch.continueWithAuth`, then `Fetch.disable`.
- Commands like `DOM.getContentQuads`, `DOM.scrollIntoViewIfNeeded`, `Page.captureScreenshot`, `Input.dispatch*` work without enabling their domains; only events need `*.enable`. Surf enables `Page` only (lifecycle/dialog events) and nothing else unless a script asks (network hooks).

Decisions that override the scratchpad (recorded in DECISIONS.md):
1. No Cranelift JIT. Bytecode VM. Scripts are IO-bound (browser is 1000x slower than any interpreter); VM gives <5 ms cold start, ~10x less code, and is the only thing that can run on wasm32 (edge target) — a JIT cannot. Revisit only with a profile.
2. Hand-written lexer (INDENT/DEDENT) + recursive-descent/Pratt parser, `ariadne` diagnostics. nom/chumsky fight indentation grammars and good error messages.
3. serde_json (+RawValue passthrough) first; simd-json only if profiling says so.
4. No `stealth:` flag. Quiet is the only mode; there is nothing to opt into. Extra flags via `flags: [...]`.
5. Concurrency: tokio current_thread + LocalSet, `Rc` values, actors are tasks with copied messages. Thousands of CDP streams on one thread is fine (Node does it); thread-per-core sharding is an additive later step.
6. Syntax: one style — indentation blocks with `:`; `#` comments; no braces/semicolons/await/let. Implicit `page(1)`; `page(n)`/`page("name")` auto-create; bare actions and `browser.x` resolve to the single page and error with multiple ("say which page"). Named browsers: `browser work:` then `work.page(2).goto(...)`.
7. Apostate is a later capability: `engine: apostate` + `persona:` map to `--fingerprint*` flags; nothing in the core depends on it.
8. Edge/WASM: design surf-vm and surf-cdp runtime-agnostic (futures + transport trait) but ship native only in v0.1.

Assumptions: MIT licence; GitHub repo heretic-tech/surf created private, flipped public at v0.1.0 tag; release binaries via cargo-dist (macOS arm64/x64, Linux x64/arm64, Windows x64).

Execution-status rule: mark each step `active` before work, `done` only after its edits and verification pass, `skipped` only with an accurate reason. Step-linked user feedback during implementation reopens that step and its dependents.

## Files
- **create** `Cargo.toml` — Workspace manifest: members crates/*, shared [workspace.dependencies] (tokio, serde, serde_json, tokio-tungstenite, ariadne, clap, tempfile, regex, dirs, tracing, thiserror, windows-sys), release profile (lto=thin, codegen-units=1, strip).
- **create** `rust-toolchain.toml` — Pin stable channel (1.97).
- **create** `README.md` — Condensed philosophy from the scratchpad (CDP is IPC, tells are side-effects, quiet by default), install, 5-line hello script, link to docs.
- **create** `AGENTS.md` — Working agreement for future agents: crate map, how to run tests with a real Chrome, the quiet-CDP rules (never Runtime.enable, never --enable-automation, always --user-data-dir), release procedure, where secrets live (none in v0.1).
- **create** `DECISIONS.md` — Record the 8 overrides above with reasons so they are not re-litigated.
- **create** `TASKS.md` — Backlog: apostate engine, WASM/edge target, CBOR pipe framing, main-world eval (addBinding technique), schedule blocks, surf fmt, imports/modules, LSP/VS Code grammar, thread-per-core sharding.
- **create** `.github/workflows/ci.yml` — cargo fmt --check, clippy -D warnings, cargo test on ubuntu (setup-chrome + xvfb) and macos; e2e job runs tests/e2e against real Chrome.
- **create** `.github/workflows/release.yml` — cargo-dist generated release workflow on v* tags.
- **create** `protocol/browser_protocol.json` — Vendored from ChromeDevTools/devtools-protocol (pin commit in protocol/VERSION).
- **create** `protocol/js_protocol.json` — Vendored companion file.
- **create** `crates/surf-cdp/Cargo.toml` — Deps: tokio, serde, serde_json, tokio-tungstenite, thiserror, tracing; build-dep for codegen.
- **create** `crates/surf-cdp/build.rs` — Generate typed command/event structs for a curated domain allowlist (Target, Page, Runtime, DOM, Input, Network, Fetch, Emulation, Browser, Storage, IO) from protocol/*.json into OUT_DIR.
- **create** `crates/surf-cdp/src/transport.rs` — `Transport` trait (send bytes / recv frames) + `pipe` (ASCIIZ framing over fd3/4 on unix, io-pipes handles on windows), `ws` (tokio-tungstenite), `tcp` (port → /json/version → ws).
- **create** `crates/surf-cdp/src/connection.rs` — Request id correlation, flat sessions (`Target.attachToTarget{flatten:true}`), `Session` handle with `call<T>()`, event broadcast per session, domain-enable tracker (refcounted enable/disable), `--trace-cdp` frame logging, protocol error type with method+message.
- **create** `crates/surf-browser/src/discover.rs` — Find a Chromium: `SURF_CHROME`, `path:` config, then macOS/Linux/Windows standard locations (Chrome, Chromium, Brave, Edge, Chrome for Testing), playwright/puppeteer caches, apostate cache. Returns path + reported version.
- **create** `crates/surf-browser/src/launch.rs` — Quiet flag set (`--remote-debugging-pipe`, `--user-data-dir`, `--no-first-run`, `--no-default-browser-check`, optional `--headless`, `--proxy-server`, `--window-size`, user `flags`), temp profile dir lifecycle, pre_exec dup2 for fd3/4 (unix) / inheritable pipe handles (windows), stderr capture for diagnostics, shutdown ladder Browser.close → SIGTERM → SIGKILL, crash detection → BrowserCrashed error.
- **create** `crates/surf-browser/src/display.rs` — `virtual: true` on Linux without DISPLAY/WAYLAND_DISPLAY: start Xvfb on first free display from :99 sized to `size`, lock file, cleanup; no-op with a note on macOS/Windows (port of apostate's xvfb.py logic, not its code).
- **create** `crates/surf-browser/src/browser.rs` — Browser handle: attach, page registry (index + name → target), `page(n)` auto-create guard, `Target.createBrowserContext` for pages with their own proxy/cookies, proxy auth via Fetch handler, close-all.
- **create** `crates/surf-browser/src/page.rs` — Page: navigation + wait (Page.enable only: lifecycleEvent load/networkIdle), isolated-world manager (lazy `Page.createIsolatedWorld` per frame, recreate on context-destroyed error), `eval`, dialogs policy, screenshot/pdf, cookies, url/title/back/reload, frames (later).
- **create** `crates/surf-browser/src/actions.rs` — Auto-waiting actions: resolve selector in isolated world → objectId → `DOM.scrollIntoViewIfNeeded` + `DOM.getContentQuads` → `Input.dispatchMouseEvent`/`dispatchKeyEvent`/`insertText`; click/type/fill/press/hover/select/check/scroll; text/html/attr/exists/count/all; wait(selector|gone|url|text|duration). Default timeout 30s, per-call override.
- **create** `crates/surf-browser/src/selector.rs` — CSS default; `text=` and `xpath=`/`//` prefixes; compiled to a single JS resolver injected once per isolated world.
- **create** `crates/surf-browser/src/network.rs` — Lazy Network/Fetch enabling: request/response hooks, block patterns, intercept (continue/fulfil/fail), disable when last hook removed.
- **create** `crates/surf-syntax/src/lexer.rs` — Tokens incl. INDENT/DEDENT/NEWLINE, numbers, duration literals (ms/s/m/h), strings with `{expr}` interpolation, `#` comments, spans.
- **create** `crates/surf-syntax/src/parser.rs` — Recursive descent + Pratt expressions. Grammar: config blocks (`browser:` / `browser name:` key: value lines), statements (assign, if/elif/else, for-in, while, loop, break/continue, fn, return, try/catch, emit, print, sleep, wait), call with keyword args, member/index access, `spawn:`/`parallel for`/`actor`/`supervisor` blocks.
- **create** `crates/surf-syntax/src/ast.rs` — AST node types with spans.
- **create** `crates/surf-syntax/src/diagnostics.rs` — ariadne-rendered errors with source excerpt; shared Diagnostic type reused by VM runtime errors (script line + CDP method).
- **create** `crates/surf-vm/src/compiler.rs` — AST → bytecode (stack VM), constants, locals/upvalues, jump patching.
- **create** `crates/surf-vm/src/vm.rs` — Async interpreter loop (`async fn run`), native calls are futures awaited in-loop, `Rc` values (nil/bool/int/float/str/list/map/duration/fn/native handle), try/catch unwinding, task spawning hook.
- **create** `crates/surf-vm/src/stdlib.rs` — strings, lists, maps, json parse/stringify, env, fs read/write/append, time now/sleep, regex, print, emit (JSON line to stdout or to parent task).
- **create** `crates/surf-runtime/src/lib.rs` — Glue: config blocks → lazy launch plans, implicit browser/page resolution rules, bare-action dispatch, native handle bindings for Browser/Page/Element, task scheduler (spawn/parallel/join), actors + supervisor restart strategies (one_for_one, one_for_all, max_restarts within duration), cancellation + browser teardown on exit.
- **create** `crates/surf-cli/src/main.rs` — clap: `surf run <file>` (also `surf <file>` and shebang), `surf check`, `surf repl`, `surf doctor` (find chrome, xvfb, pipe test), `surf install` (Chrome for Testing download to ~/.cache/surf), flags `--trace-cdp`, `--json`, `--timeout`.
- **create** `examples/hello.surf` — page.goto + text + print (3 lines).
- **create** `examples/login.surf` — The scratchpad's comparison example.
- **create** `examples/two-tabs.surf` — page(1)/page(2)/page("login") indexing.
- **create** `examples/scrape-emit.surf` — for-in over links, emit JSON lines, run with --json.
- **create** `examples/parallel-pool.surf` — parallel for over URLs, each on its own page with its own proxy context.
- **create** `examples/supervised.surf` — actor + supervisor restart demo.
- **create** `tests/e2e/server.rs` — axum fixture server serving tests/fixtures/*.html (forms, delayed elements, dialogs, auth proxy stub).
- **create** `tests/e2e/scripts/` — One .surf per feature with expected stdout; runner executes `surf run` against the fixture server with a real Chrome.
- **create** `tools/detector/index.html` — Local replica of known checks: Runtime.enable leak (console getter + Error.stack), navigator.webdriver, headless UA, window.chrome, isolated-world globals absent from main world. Served by the e2e server; a .surf test asserts all pass.
- **create** `docs/language.md` — Full language reference: lexical rules, config blocks, implicit page rules, actions, selectors, waits, concurrency, errors.
- **create** `docs/architecture.md` — Crate map + mermaid of script → VM → runtime → CDP → transport.
- **create** `docs/quiet-cdp.md` — Exactly which flags/domains Surf sends and why, with the detector results table.
- **create** `docs/comparison.md` — Side-by-side with Playwright/Puppeteer/chromedp/chromiumoxide.
- **create** `.gitignore` — target/, .empryo/ stays tracked? no — ignore .empryo/*.db and sessions; keep .empryo/launch.json if created.
- **create** `LICENSE` — MIT.

## Steps
### step-1. Scaffold workspace, docs skeleton, CI, GitHub repo

Files: `Cargo.toml`, `rust-toolchain.toml`, `README.md`, `AGENTS.md`, `DECISIONS.md`, `TASKS.md`, `.gitignore`, `LICENSE`, `.github/workflows/ci.yml`, `protocol/browser_protocol.json`, `protocol/js_protocol.json`

```sh
cargo build && cargo clippy -- -D warnings
```

Create 6 crates (surf-syntax, surf-vm, surf-cdp, surf-browser, surf-runtime, surf-cli) with empty lib/main; vendor protocol JSON (pin commit in protocol/VERSION); write DECISIONS.md with the 8 overrides; TASKS.md backlog; `gh repo create heretic-tech/surf --private`; initial commit. Verify: `cargo build` green.

### step-2. surf-cdp: transports, framing, sessions, codegen

Files: `crates/surf-cdp/build.rs`, `crates/surf-cdp/src/transport.rs`, `crates/surf-cdp/src/connection.rs`

```sh
cargo test -p surf-cdp
```

Pipe transport (ASCIIZ over fd3/4; windows io-pipes handles), ws transport, tcp→/json/version→ws. Connection: id correlation, flat sessions, per-session event streams, refcounted domain-enable tracker, trace logging. build.rs codegen for the domain allowlist. Unit tests with a fake transport (framing split across reads, interleaved events). Risk: Windows handle inheritance — implement via windows-sys CreatePipe + SetHandleInformation; mark as needs-Windows-CI if untestable locally.

### step-3. surf-browser: discovery, quiet launch, profiles, Xvfb, shutdown

Files: `crates/surf-browser/src/discover.rs`, `crates/surf-browser/src/launch.rs`, `crates/surf-browser/src/display.rs`

```sh
cargo test -p surf-browser --test launch -- --nocapture
```

Quiet flag set only; always a user-data-dir (Chrome>=136); pre_exec dup2 for fd3/4; stderr capture; shutdown ladder; crash detection; Xvfb manager on Linux. Integration test: launch local Google Chrome over pipe, `Browser.getVersion`, close, assert temp dir removed and no listening port. Verify navigator.webdriver===false and no infobar (outerHeight-innerHeight sanity).

### step-4. surf-browser: pages, isolated worlds, auto-wait actions, selectors, contexts

Files: `crates/surf-browser/src/browser.rs`, `crates/surf-browser/src/page.rs`, `crates/surf-browser/src/actions.rs`, `crates/surf-browser/src/selector.rs`

```sh
cargo test -p surf-browser
```

Page registry with auto-create guard; navigation wait via Page lifecycle events (Page is the only enabled domain); isolated-world manager with lazy recreate; eval returns JSON values; actions through DOM quads + Input events; auto-wait predicate loop (attached/visible/stable/enabled) with timeouts; per-page browser contexts with proxyServer + Fetch auth. Tests against the axum fixture server (step-9 server created here minimally).

### step-5. surf-syntax: lexer, parser, AST, diagnostics, `surf check`

Files: `crates/surf-syntax/src/lexer.rs`, `crates/surf-syntax/src/parser.rs`, `crates/surf-syntax/src/ast.rs`, `crates/surf-syntax/src/diagnostics.rs`, `docs/language.md`

```sh
cargo test -p surf-syntax
```

Write docs/language.md grammar first, then implement to it. Indentation tokens, duration literals, string interpolation, keyword args, config blocks, all statement forms incl. spawn/parallel/actor/supervisor. insta snapshot tests for AST and for error rendering (bad indent, unclosed string, unknown keyword suggestion).

### step-6. surf-vm: bytecode compiler, async interpreter, stdlib

Files: `crates/surf-vm/src/compiler.rs`, `crates/surf-vm/src/vm.rs`, `crates/surf-vm/src/stdlib.rs`

```sh
cargo test -p surf-vm
```

Stack VM, Rc values, closures, try/catch, loops, native-call trait returning futures, emit/print. Fixture tests: tests/vm/*.surf with expected stdout, no browser. Cold-start bench: parse+compile+run of hello without browser < 5 ms.

### step-7. surf-runtime + surf-cli: implicit browser/page, `surf run`, examples

Files: `crates/surf-runtime/src/lib.rs`, `crates/surf-cli/src/main.rs`, `examples/hello.surf`, `examples/login.surf`, `examples/two-tabs.surf`, `examples/scrape-emit.surf`

```sh
cargo run -- run examples/hello.surf
```

Config block → lazy launch on first action; resolution rules (bare/browser.x → single page else friendly error naming the pages); named browsers; `wait` forms; runtime errors carry script line + CDP method; teardown on exit/Ctrl-C. `surf run`, shebang support, `--trace-cdp`, `--json`. Run all four examples end-to-end against the fixture server and example.com.

### step-8. Concurrency: spawn, parallel for, actors, supervisors

Files: `crates/surf-runtime/src/lib.rs`, `crates/surf-vm/src/vm.rs`, `examples/parallel-pool.surf`, `examples/supervised.surf`

```sh
cargo test -p surf-runtime
```

spawn: fire-and-forget task; parallel for with join + error aggregation; actor definitions with receive:, send(); supervisor with strategy one_for_one|one_for_all, max_restarts within duration, child specs; each task gets its own page by default; cancellation propagates. Test: 50 parallel pages in one browser on the fixture server; supervisor restarts a crashing actor 3x then gives up with a clear error.

### step-9. Network hooks, blocking, intercept, cookies; e2e suite

Files: `crates/surf-browser/src/network.rs`, `tests/e2e/server.rs`, `tests/e2e/scripts/`

```sh
cargo test --test e2e
```

on request/on response, block patterns, intercept fulfil/fail/continue, cookies get/set/export/import JSON, downloads dir. Network/Fetch enabled only while hooks exist. E2E runner executes every tests/e2e/scripts/*.surf against the fixture server with real Chrome (headed under Xvfb on CI).

### step-10. Detection + perf gates, doctor, repl, install

Files: `tools/detector/index.html`, `crates/surf-cli/src/main.rs`, `docs/quiet-cdp.md`

```sh
cargo run -- run tests/e2e/scripts/detector.surf
```

Local detector page replicating Runtime.enable leak, webdriver, headless UA, isolated-world leakage; e2e asserts all green. Manual live run against bot-detector.rebrowser.net, bot.sannysoft.com, browserscan.net — record results in docs/quiet-cdp.md. `surf doctor`, `surf repl`, `surf install` (Chrome for Testing JSON API → ~/.cache/surf). Benches: surf process start→first CDP frame < 10 ms; idle RSS < 15 MB.

### step-11. Docs, release pipeline, v0.1.0

Files: `docs/architecture.md`, `docs/comparison.md`, `README.md`, `.github/workflows/release.yml`, `AGENTS.md`

```sh
cargo dist build && git tag v0.1.0
```

Finish docs; cargo-dist init for 5 targets; CI green on ubuntu+macos; tag v0.1.0; flip repo public; GitHub release with binaries; update AGENTS.md with release procedure and TASKS.md backlog (apostate engine, WASM/edge, CBOR, main-world eval, schedule, fmt, modules). Conventional commits throughout; commit after every step.

## Verification
- cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test (unit: lexer/parser snapshots, VM fixtures, CDP framing)
- cargo test --test e2e — every tests/e2e/scripts/*.surf passes against the fixture server with real Google Chrome over the pipe transport
- tests/e2e/scripts/detector.surf: navigator.webdriver false, no Runtime.enable leak, no isolated-world globals in main world, no listening TCP port while a browser is up (lsof -iTCP -sTCP:LISTEN -p <chrome pid> empty)
- surf run examples/*.surf all succeed; examples/login.surf is byte-for-byte the scratchpad example
- Cold start: hyperfine 'surf run tests/vm/noop.surf' < 10 ms; idle surf RSS < 15 MB with one browser attached
- Live detector pass recorded in docs/quiet-cdp.md (rebrowser bot-detector, sannysoft, browserscan) with date + Chrome version
- CI green on ubuntu (xvfb) and macos; cargo-dist produces 5 target archives; v0.1.0 release published