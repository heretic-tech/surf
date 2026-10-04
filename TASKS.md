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
- [ ] Windows pipe transport via `--remote-debugging-io-pipes` (handles).

## Protocol
- [ ] CBOR pipe framing (`--remote-debugging-pipe=cbor`) behind a profile
      showing JSON on the hot path. (Decision 3)
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

## Tooling
- [ ] LSP server (`surf lsp`) + VS Code extension with a TextMate grammar.
- [ ] `surf doctor` full report: Chrome version, flags used, pipe round-trip
      latency, `navigator.webdriver` check, isolated-world sanity.
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
- `protocol/generated.rs` is a hand-written placeholder; task 2 replaces it
  with codegen output (build.rs or `cargo xtask codegen`).
