# Agent: bg-2 (code)
Task: # SURF — shared brief for every node (read fully before acting)

Repo: /Users/chaser/surf. Host: macOS arm64, cargo 1.97 stable, Google Chrome at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`, Playwright Chromium builds under ~/Library/Caches/ms-playwright, Apostate (a Chromium fork

## Tool Calls
[read] 
[shell]
[read] 
[grep] /surf_cdp::|WebSocketTransport|ChildEnds|create_pair|PipeTransport|generated::/
[shell]
[shell]
[shell]
[shell]
[edit_file] crates/surf-cdp/Cargo.toml
[edit_file] crates/surf-cdp/src/transport/mod.rs
[edit_file] crates/surf-cdp/src/transport/pipe.rs
[edit_file] crates/surf-cdp/src/transport/pipe.rs
[edit_file] crates/surf-cdp/src/transport/ws.rs
[edit_file] crates/surf-cdp/src/transport/tcp.rs
[edit_file] crates/surf-cdp/src/connection.rs
[multi_edit] crates/surf-cdp/src/connection.rs
[edit_file] crates/surf-cdp/src/session.rs
[edit_file] crates/surf-cdp/src/event.rs
[edit_file] crates/surf-cdp/build.rs
[multi_edit] crates/surf-cdp/build.rs
[edit_file] crates/surf-cdp/src/protocol/mod.rs
[multi_edit] crates/surf-cdp/build.rs
[multi_edit] crates/surf-cdp/src/lib.rs
[shell]
[read] 
[multi_edit] crates/surf-cdp/build.rs
[edit_file] crates/surf-cdp/src/transport/fake.rs
[edit_file] crates/surf-cdp/src/tests.rs
[multi_edit] crates/surf-cdp/src/transport/mod.rs
[multi_edit] crates/surf-cdp/src/lib.rs
[shell]
[edit_file] crates/surf-cdp/src/protocol/mod.rs
[shell]
[shell]
[shell]
[shell]
[grep] /closed_tx.send/
[edit_file] crates/surf-cdp/src/connection.rs
[edit_file] crates/surf-cdp/src/transport/ws.rs
[edit_file] crates/surf-cdp/tests/chrome_pipe.rs
[shell]
[multi_edit] crates/surf-cdp/tests/chrome_pipe.rs
[shell]
[multi_edit] crates/surf-cdp/src/transport/ws.rs
[shell]
[shell]
[shell]
[multi_edit] docs/architecture.md
[multi_edit] TASKS.md
[shell]
[shell]
[shell]

## Findings
**surf-cdp API (step 2 done) — what surf-browser can rely on:**
Committed 47b0513..48c3c83. Public surface (crates/surf-cdp):
- `transport::pipe::create_pair() -> io::Result<(PipeTransport, ChildFds)>`. Unix: `ChildFds { read: OwnedFd /* child fd 3 */, write: OwnedFd /* child fd 4 */ }`, `.raw() -> (RawFd, RawFd)`. Both are CLOEXEC + blocking; launcher must `dup2(read,3); dup2(write,4)` in `pre_exec` (unsafe lives in surf-browser) and drop `ChildFds` after `spawn`. Working example in crates/surf-cdp/tests/chrome_pipe.rs (runs against real Chrome 154 in 0.5 s). Windows: `ChildFds::io_pipes_arg()` gives `--remote-debugging-io-pipes=R,W`.
- `transport::ws::WsTransport::connect(url)`; `transport::tcp::connect(host, port)` (GET /json/version → ws). `transport::fake::pair()` = channel-backed transport for unit tests (test plays the browser: `next_sent()`, `respond(id, v)`, `event(method, params, session)`).
- `Connection::new(Box<dyn Transport>)` (needs a tokio runtime; spawns one driver task). `root()`, `attach(target_id)` (flatten:true), `session(id)` for ids from Target.attachedToTarget, `close()`, `wait_closed()`, `is_closed()`, `set_trace()`; env `SURF_TRACE_CDP=1`.
- `Session::call/call_raw/send::<C: Command>()`, `with_timeout(Option<Duration>)` → `CdpError::Timeout{method}`, `events(method)` (broadcast cap 1024, `"*"` catch-all; use `surf_cdp::event::next(&mut rx)` to drain with lag logging), `enable_domain("Page") -> DomainGuard` (refcounted; Runtime/DOM refused), `domain_refcount()`, `detach()` (sends Target.detachFromTarget and closes the session's event channels; also closed automatically on Target.detachedFromTarget).
- Typed protocol: `surf_cdp::protocol::{target, page, runtime, dom, input, network, fetch, emulation, browser, storage, io, security}`; `page::Navigate { url, ..Default::default() }` → `session.send(..)` → `NavigateResponse`; events `page::LoadEventFiredEvent::METHOD` + `Event::parse::<E>()`. No `runtime::Enable`/`dom::Enable` exist; `FORBIDDEN_METHODS` refused at runtime in all builds.
- Verified in the Chro

## Agent Summary
`surf-cdp` implemented and committed (5 commits, `47b0513..48c3c83`); surf-syntax working tree untouched by me (only their in-progress edits remain unstaged).

**Built**
- `transport/pipe.rs` — `create_pair() -> (PipeTransport, ChildFds)` using `tokio::net::unix::pipe` (no `unsafe` on unix: child ends via `into_blocking_fd`, CLOEXEC kept, launcher `dup2`s onto 3/4 in `pre_exec`, documented). `\0` decoder handles split/coalesced frames; `recv` is cancel-safe. Windows `CreatePipe`+`SetHandleInformation` variant behind `#[cfg(windows)]` with `SAFETY:` comments (`#![cfg_attr(windows, deny(unsafe_code))]`, forbid elsewhere).
- `transport/ws.rs` `WsTransport::connect` (tungstenite, limits lifted, reset-without-close = EOF); `transport/tcp.rs` raw `GET /json/version` → ws; `transport/fake.rs` channel-backed transport for tests.
- `connection.rs` — single driver task (`select!` outgoing vs `recv`), id correlation via `oneshot<RawValue>`, `(sessionId, method)` broadcast routing (cap 1024, `warn!` on full, `"*"` catch-all, channels closed on detach/`Target.detachedFromTarget`), `attach`, `close`/`wait_closed`, `SURF_TRACE_CDP`/`set_trace` with `→`/`←` frames (4 KiB truncation). Driver holds `Weak`, so dropping the last handle closes the pipe.
- `session.rs` — `call/call_raw/send`, `with_timeout` → `CdpError::Timeout{method}`, `FORBIDDEN_METHODS` refused in all builds, `DomainGuard` refcount (serialised enable, best-effort spawned disable with re-check).
- `build.rs` codegen for the 12-domain allow-list → `protocol::{target, page, …}` (`Command`/`ProtocolEvent` impls, `Unrecognized` enum catch-all, boxed recursion; `Runtime.enable`/`DOM.enable` not generated). ~33 k generated lines, ~3 s clean build.

**Measured** — `cargo test --workspace`: 28 surf-cdp unit tests + ws echo + `cat` pipe roundtrip; `tests/chrome_pipe.rs` ran against real Chrome 154 (0.5 s): root call, typed `createTarget`, attach, `Page.enable` guard, `loadEventFired`, isolated world + `Runtime.evaluate{contextId}`, **`Runtime.addBinding` → `bindingCalled` without `Runtime.enable`**, detach, `Browser.close` → EOF. `cargo clippy --workspace --all-targets -D warnings` clean, fmt clean.

**Deferred (TASKS.md)** — Windows pipe needs Windows CI; split read/write transport if the recreate-recv-per-send shows in a profile; event-channel sweep for idle sessions; launcher must handle child fd already being 3/4.
