# Agent: bg-8 (explore)
Task: # SURF — shared brief for every node (read fully before acting)

Repo: /Users/chaser/surf. Host: macOS arm64, cargo 1.97 stable, Google Chrome at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`, Playwright Chromium builds under ~/Library/Caches/ms-playwright, Apostate (a Chromium fork

## Tool Calls
[grep] /Runtime\.enable|enable-automation|AutomationControlled|DOM\.enable/
[grep] /tokio|std::process|std::fs/
[grep] /tokio|std::process|std::fs/
[read] 
[read] 
[grep] /tokio|std::process|std::fs/
[grep] /Declaration::(Handler|Task|Actor|Supervisor|Config|Fn)|host\.declare|is_catchable|kwargs/
[grep] /pub (trait|struct|enum|fn|async fn)/
[read] 
[read] 
[grep] /pub fn new\(|pub fn fork|pub async fn run|pub async fn call\(|pub fn call_value|pub fn cancel_token|pub fn compile|FORBIDDEN_METHODS/
[grep] /try:|catch/
[grep] /CdpMode::Pipe|cdp: CdpMode|Default for CdpMode|TransportChoice::Pipe/
[grep] /catch/
[grep] /Declaration::|declare/
[project]
[final_response]

## Agent Summary
Review of the four foundation crates:

1. Tests green per supplied output; `cargo clippy --workspace --all-targets -D warnings` re-run now — clean.
2. Quiet rules: repo-wide grep for `Runtime.enable|enable-automation|AutomationControlled|DOM.enable` hits only docs, comments, `FORBIDDEN_METHODS` (`surf-cdp/src/lib.rs:59`, enforced at `connection.rs:202-208` for every call path incl. typed `send`), `enable_domain` refusal (`session.rs:108-114`), and the single parent-approved Decision 12 switch at `launch.rs:396-402` (guarded by tests asserting no other `--disable-*`/`--enable-automation`). `--user-data-dir` unconditional at `launch.rs:393`; `CdpMode::Pipe` is `LaunchOptions::default()` (`launch.rs:114`) and `LaunchConfig::for_path` (`:374`).
3. Contracts: `docs/architecture.md:91-127` and `:246-317` match real signatures field-for-field (`Transport` `mod.rs:33-38`, `CdpError` `error.rs:7-41`, `Session` `session.rs:49-113`, `Value`/`Args`/`NativeObject` `value.rs:16-37,296-300,377-412`, `Host`/`Declaration`/`Prop` `host.rs:20-185`, `RuntimeError` `error.rs:36-137`, `Vm` `vm.rs:118-185`, `compile`/`compile_with_source` `compiler.rs:56-62`). Doc already documents the `self: &Arc<Self>` receiver on `root`/`attach` (`:130`).
4. `surf-syntax/Cargo.toml` deps: ariadne + serde only; `surf-vm/Cargo.toml`: surf-syntax, futures, indexmap, serde_json. Grep for `tokio|std::process|std::fs` in both `src/` trees hits only doc comments. wasm32-clean.
5. VM: `vm.rs:755-756` builds and hands every `DeclKind` (Config/Fn/Task/Actor/Supervisor/Handler, `:845-879`) to `host.declare`; `tests/fixtures.rs:108-163` consumes all six and snapshots them. try/catch: `run_frames` `vm.rs:381-395` unwinds to the handler for any `is_catchable()` error, including those returned by native `call_method` (`:545`) and host `call_global` (`:531`); `errors.surf:1-4,35-38` cover a native-action failure and an error thrown through `[1].map(...)`. kwargs: `pop_args` `vm.rs:234-254` builds `Args.kwargs` and passes them unchanged to natives/globals/host (`:511-546`).
6. CDP: `FrameDecoder::next_frame` `framing.rs:33-40` scans for `\0` across partial feeds and yields multiple frames from one buffer (test `:53-65`). `DomainGuard` refcount `session.rs:115-139` / drop `:217-257` with serialised re-check. `CdpError::Protocol` and `Timeout` both carry `method` (`error.rs:10-17,37-40`; populated at `connection.rs:203-207`, `session.rs:86-88`).

Cosmetic only: `protocol/mod.rs:16-17` names the catch-all `Unknown` vs generated `Unrecognized`; `libc` should be a dev-dependency in `surf-cdp/Cargo.toml`.

VERDICT: PASS — all four crates satisfy quiet rules, documented contracts, wasm32-cleanliness, VM declaration/try-catch/kwargs semantics and CDP framing/DomainGuard/error requirements; only two cosmetic nits.
