# Backlog

Deferred work, in rough priority order. Add an entry when you defer
something; link the PR / commit that picks it up.

## Engine / fingerprinting
- [ ] `engine: apostate` — launch the Apostate fork (discovery via
      `~/Library/Caches/apostate` / `SURF_APOSTATE`), clear "not yet" error
      until then. (Decision 7)
- [ ] `persona:` on `browser:` and per page — per-page fingerprints via
      process-per-persona + `Page::rebind`; a logical `Browser` owning several
      OS processes. (Decisions 7, 10)

## Platforms / targets
- [ ] WASM / edge target: build `surf-syntax` + `surf-vm` + a websocket-only
      runtime for `wasm32-unknown-unknown`; CDP over `pool:` only. (Decision 8)
- [ ] Thread-per-core sharding: multi-threaded runtime where each thread owns
      a set of browsers; `surf-cdp` is already `Send`. (Decision 5)
- [ ] Windows pipe transport: `transport::pipe::windows` (CreatePipe +
      SetHandleInformation, `--remote-debugging-io-pipes=<read>,<write>`) is
      written and compiles behind `#[cfg(windows)]` but has never run — needs
      a Windows CI job (`cargo test -p surf-cdp` there, plus the Chrome pipe
      e2e). Parent ends are `tokio::fs::File` (blocking pool); revisit if
      latency matters.

## Protocol
- [ ] CBOR pipe framing (`--remote-debugging-pipe=cbor`) behind a profile
      showing JSON on the hot path. (Decision 3)
- [ ] surf-cdp: the single driver task re-creates `Transport::recv` on every
      outgoing frame (cancel-safe by contract). If a profile shows this on the
      hot path, add a split read/write transport (two tasks) — the public
      `Transport` trait would gain an optional `split()`.
- [ ] surf-cdp: `Session::events` channels are created lazily and dropped
      only when their last receiver is gone *and* an event arrives; add an
      explicit `unsubscribe`/sweep if idle sessions pile up (parallel pools).
- [ ] surf-browser launcher: port mode (`cdp: N`) connects through
      `surf_cdp::transport::tcp::connect`, which has no timeouts of its own;
      the launcher wraps each attempt in 3 s. Fold a timeout into `tcp` if
      attach mode (`cdp: "ws://…"`, step 7) needs one too.
- [ ] surf-browser launcher: Windows path (`--remote-debugging-io-pipes`,
      no `SIGTERM` → straight to `TerminateProcess`) is written but has
      never run; needs the Windows CI job above.
- [ ] surf-browser launcher: Linux `display.rs` (Xvfb) compiles only on
      Linux and was not exercised on this Mac; CI's `xvfb-run` job sets
      `DISPLAY`, so `virtual: true` is a no-op there. Add a Linux job that
      unsets `DISPLAY` and runs a `virtual: true` launch.
- [ ] Main-world eval (opt-in, explicit): `addBinding` technique — install a
      binding in an isolated world, use a page-side trampoline so main-world
      code never sees `Runtime.enable`. Needs a design note on the observable
      side effects before implementation.
- [ ] Frames API: `frame("name")` / iframe targeting with isolated worlds per
      frame; `Page.frameAttached` tracking.

## Language
- [ ] `schedule:` blocks (cron-like: `schedule every 5m:` / `schedule at
      "09:00":`).
- [ ] Modules / imports (`use "./lib.surf"`), with a module cache and
      diagnostics spanning files.
- [ ] Compound assignment (`+=`), string methods as methods (`s.upper()`).
- [ ] `surf fmt` — canonical formatter driven by the AST (spans preserved).
- [ ] surf-syntax: block lambdas inside brackets (`apply(fn(x):` + indented
      body as an argument). Newlines are suppressed inside `(…)`, so the
      lexer cannot see the block; needs layout-aware bracket handling.
- [ ] surf-syntax: `eval(fn(): …)` bodies are parsed as Surf today (the
      runtime slices `LambdaBody::Expr(e).span` from the source to get the
      verbatim JS). Decide whether to add a raw-JS lambda form
      (`js(…)`/backticks) for expressions Surf cannot parse (`=>`, `?.`).
- [ ] surf-syntax: keyword-adjacent tokens with no space (`1if`, `x.in`)
      lex as number + keyword / error; fine for now, revisit with `surf fmt`.
- [ ] surf-syntax: the known-key lists for `browser:` / task / supervisor /
      `parallel for` props and handler events live in `parser.rs`
      (`BROWSER_PROPS`, …). When the runtime adds a key, update the list and
      `docs/language.md` together (consider generating one from the other).
- [ ] surf-syntax: a statement that starts with a list literal
      (`[1, 2].each(f)`) fails to parse (`expected `]`, found `,``); the
      statement parser treats a leading `[` as something else. Assign to a
      name first as a workaround.
- [ ] surf-vm: `spawn` of a non-global callee (`f = fn(): …; spawn f()`) is
      a compile error; only `spawn name(…)` (→ `Host::spawn`) and
      `spawn recv.m(…)` (→ `Host::spawn_method`) exist. Add a
      `Host::spawn_value` if scripts need it.
- [ ] surf-vm: parameter defaults are limited to the first 64 parameters
      (a `u64` "missing" mask per frame); more parameters silently treat
      the default as supplied.
- [ ] surf-vm: `now()` and `random()` seed from `std::time::SystemTime`
      directly (compiles on wasm32 but panics there at runtime); route
      through a `Host::now` when the wasm build lands.
- [ ] surf-vm: reads of a function-scoped variable before its first
      assignment yield `nil` (the pre-scan allocates the slot); consider an
      "unassigned" marker to report `used before assignment` instead.
- [ ] surf-vm: `Op` is an enum of up to 16 bytes; if a profile ever shows
      dispatch cost, pack to u32 words. Not worth it for IO-bound scripts.

## Tooling
- [ ] LSP server (`surf lsp`) + VS Code extension with a TextMate grammar.
- [ ] `surf doctor` full report: Chrome version, flags used, pipe round-trip
      latency, `navigator.webdriver` check, isolated-world sanity. Switch it
      from the path-only `surf_browser::find_chrome()` wrapper to
      `surf_browser::discovery::find_chrome(None)` (version, origin,
      `search_locations()`), then delete the wrapper.
- [ ] `surf install`: download Chrome for Testing into
      `~/.cache/surf/chrome/<version>/` (discovery already walks that tree,
      newest version first).
- [ ] Detection gate (step 10): assert `navigator.webdriver === false` on a
      launched browser and keep
      `tests/launch.rs::webdriver_is_true_without_automationcontrolled_switch`
      as the control (Decision 12). If a future Chrome stops setting the
      flag for a debugger pipe, that control test fails and the switch can
      be reconsidered.
- [ ] `surf repl`.
- [ ] Detection + perf gates in CI (cold start < 5 ms VM, launch-to-first-
      action budget, zero forbidden methods in a CDP trace).

## Scaffold notes (step 1)
- The wasm32 build needs the rustup toolchain's `rustc`; on a machine where
  a non-rustup `cargo`/`rustc` shadows `~/.cargo/bin` (MacPorts here), run
  `RUSTC=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc
  $HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo build -p
  surf-syntax -p surf-vm --target wasm32-unknown-unknown`. CI uses rustup
  directly.
- `protocol/generated.rs` placeholder was replaced in step 2 by
  `crates/surf-cdp/build.rs` codegen (`$OUT_DIR/protocol.rs`, ~33 k lines
  for the 12-domain allow-list; adds ~3 s to a clean `surf-cdp` build).
