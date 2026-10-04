# Agent: bg-4 (code)
Task: # SURF — shared brief for every node (read fully before acting)

Repo: /Users/chaser/surf. Host: macOS arm64, cargo 1.97 stable, Google Chrome at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`, Playwright Chromium builds under ~/Library/Caches/ms-playwright, Apostate (a Chromium fork

## Tool Calls
[shell]
[read] 
[read] 
[read] 
[grep] /find_chrome|chrome_or_skip|LaunchOptions|CdpMode|xvfb::|Xvfb|strip_credentials|proxy_credentials|surf_browser::/
[read] 
[shell]
[shell]
[shell]
[multi_edit] crates/surf-cdp/src/connection.rs
[shell]
[read] 
[edit_file] crates/surf-cdp/src/tests.rs
[edit_file] crates/surf-browser/src/error.rs
[edit_file] crates/surf-browser/src/error.rs
[edit_file] crates/surf-browser/src/discovery.rs
[multi_edit] crates/surf-browser/src/discovery.rs
[shell]
[edit_file] crates/surf-browser/src/display.rs
[shell]
[edit_file] crates/surf-browser/src/launch.rs
[multi_edit] crates/surf-browser/src/launch.rs
[multi_edit] crates/surf-browser/src/lib.rs
[shell]
[edit_file] crates/surf-browser/src/launch.rs
[shell]
[edit_file] crates/surf-browser/tests/launch.rs
[shell]
[shell]
[edit_file] crates/surf-browser/tests/launch.rs
[shell]
[edit_file] crates/surf-browser/tests/scratch_webdriver.rs
[shell]
[shell]
[shell]
[shell]
[read] 
[multi_edit] crates/surf-cdp/src/transport/tcp.rs
[multi_edit] crates/surf-browser/src/launch.rs
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
[notify_parent]
[multi_edit] crates/surf-browser/tests/launch.rs
[read] 
[multi_edit] crates/surf-browser/tests/launch.rs
[shell]
[shell]
[shell]
[edit_file] crates/surf-browser/tests/launch.rs
[shell]
[shell]
[shell]
[shell]
[shell]
[shell]
[read] 
[multi_edit] crates/surf-browser/src/launch.rs
[grep] /fn flags_are_quiet_and_in_order|--disable-|fn port_mode_and_user_disable_flags_pass_through/
[read] 
[edit_file] crates/surf-browser/src/launch.rs
[read] 
[multi_edit] crates/surf-browser/tests/launch.rs
[shell]
[shell]
[edit_file] DECISIONS.md
[multi_edit] AGENTS.md
[multi_edit] docs/architecture.md
[multi_edit] TASKS.md
[grep] /AutomationControlled|--disable-\*|disable-blink|LaunchOptions::args|xvfb/
[shell]
[shell]
[shell]
[shell]
[read] 
[multi_edit] crates/surf-browser/src/display.rs
[shell]
[multi_edit] crates/surf-browser/src/display.rs
[shell]
[shell]
[shell]
[shell]

## Findings
**navigator.webdriver is true on stock Chrome whenever remote debugging is on (measured):**
Measured on this Mac with Google Chrome 154.0.8037.95 and Chrome for Testing 133/152, NO CDP session attached, pure CLI:
  chrome --headless --dump-dom 'data:…navigator.webdriver…'                          → wd=false
  chrome --headless --remote-debugging-port=0 --dump-dom …                           → wd=true
  chrome --headless --enable-automation --dump-dom …                                 → wd=true
Via surf_browser::launch (quiet flag set only: pipe/port, user-data-dir, no-first-run, no-default-browser-check, window-size, [headless], about:blank), target created with Target.createTarget and title read with Target.getTargets (no attach, no Runtime.*): headed pipe / headed port / headless pipe / headless port ALL report navigator.webdriver === true.
So: in current Chromium, `--remote-debugging-pipe` AND `--remote-debugging-port` themselves enable Blink's AutomationControlled runtime feature; `--enable-automation` is not the only trigger. The ONLY things that turn it off are `--disable-blink-features=AutomationControlled` (forbidden by quiet rule 2) or a patched binary (Apostate, Decision 7). The infobar is NOT shown (outerHeight−innerHeight = 87 px headed at 1280x800).
Consequence for step 10 (detection gates) and step 7/9 docs: a `navigator.webdriver === false` gate cannot pass on stock Chrome without amending quiet rule 2. The surf-browser launch test asserts no infobar and records webdriver; it does not assert it false. Parent notified.

**surf-browser launch API landed (step 3) + Decision 12 flag:**
Commits 2e58699 (cdp) and ea1a5b7 (browser). For step 4 / step 7 implementers:
- `surf_browser::launch::LaunchOptions::resolve()` → `LaunchConfig` (discovery, headed/headless decision, ProxySpec parse); `launch(cfg).await` → `Launched { connection: Arc<Connection>, process, profile_dir, display, proxy_credentials: Option<Credentials>, product, path, args }`; `Launched::root()`, `stderr_tail()`, `close().await` (idempotent; call from Ctrl-C). Attach mode (`CdpMode::Attach`) is NOT handled by `launch` — `resolve()` errors; step 7 should build the Connection from `surf_cdp::transport::ws` directly for attach/pool and never add Surf flags there.
- Flag list lives ONLY in `LaunchConfig::args`. Decision 12 (parent-approved): `--disable-blink-features=AutomationControlled` is passed on launched browsers (Chrome sets navigator.webdriver=true for any configured debugger pipe/port). Any other `--disable-*` or `--enable-automation` stays forbidden.
- `surf_cdp::Connection::mark_crashed(exit_code, stderr_tail)` is new; `CdpError::BrowserCrashed` now actually surfaces to callers when Chrome dies.
- `surf_cdp::transport::tcp::http_get` fixed: Chrome keeps `/json/version` open; read stops at Content-Length.
- `surf_browser::find_chrome()` (root, path-only) still exists for surf-cli's doctor; prefer `discovery::find_chrome(None)` (version, origin) / `discovery::search_locations()`.
- Module `xvfb` renamed to `display` (`VirtualDisplay`, `has_display()`).

## Agent Summary
**Quiet rule 2 amended (Decision 12).** Stock Chrome sets `navigator.webdriver = true` whenever a debugger pipe *or* port is configured — no `--enable-automation`, no session attached. Measured on Google Chrome 154.0.8037.95 stable and Chrome for Testing 133/152: `chrome --headless --dump-dom` with no debugging flag → `wd=false`; same command + `--remote-debugging-port=0` → `wd=true`; through `surf_browser::launch` with the quiet set, all four pipe/port × headed/headless combinations → `true` (infobar absent: 87 px at 1280×800). With the parent's approval Surf now passes exactly one `--disable-*` on browsers it **launches**: `--disable-blink-features=AutomationControlled` (`launch.rs` carries `// see DECISIONS.md #12` + https://issues.chromium.org/issues/40746300). `--enable-automation` and every other `--disable-*` remain forbidden; nothing is added when attaching. `tests/launch.rs::webdriver_is_true_without_automationcontrolled_switch` launches without it and asserts `true`, so the reason can't be cleaned away. Recorded in DECISIONS.md #12, AGENTS.md rule 2, docs/architecture.md.

**Built** (commits `2e58699` cdp, `ea1a5b7` browser):
- `discovery.rs` — `find_chrome(explicit) -> Result<Found{path, version, origin}>`; order explicit → `SURF_CHROME` (both hard errors if set-but-missing) → `~/.cache/surf/chrome/**` → platform/PATH → Playwright/Puppeteer/Apostate caches via a bounded, pruned walk (newest version first, no tool layouts hard-coded); `--version` under 3 s; `NotFound` lists every location tried. Root `surf_browser::find_chrome()` kept path-only for surf-cli.
- `launch.rs` — `LaunchOptions::resolve()` → `LaunchConfig` (`TransportChoice{Pipe|Port}`, `ProxySpec::parse` for `http://u:p@h:p | socks5://h:p | h:p`, credentials kept on `Launched::proxy_credentials`); `LaunchConfig::args` is the single flag list; `ProfileDir` (temp removed with retries / `profile:` kept); fd 3/4 via `pre_exec` in `launch::sys` (crate's only `unsafe`, handles fds already at 3/4); port 0 via `DevToolsActivePort`; 64-line stderr ring; `Browser.getVersion` within 10 s; watcher task → `Connection::mark_crashed` (transport wrapper delays EOF ≤1 s so in-flight calls get `BrowserCrashed`, verified by the bogus-binary test: `exit code Some(7): fake chrome: refusing`); `Launched::close()` ladder `Browser.close → 2s → SIGTERM → 2s → SIGKILL`, idempotent.
- `display.rs` (renamed from `xvfb.rs`) — Linux Xvfb `:99+`, lock/socket probe, 5 s socket wait, `DISPLAY` child-only; no-op + info elsewhere. Cross-checked with `clippy --target x86_64-unknown-linux-gnu` (caught a `tracing` `{display}` field-name clash).
- surf-cdp additions: `Connection::mark_crashed`; `tcp::http_get` now stops at `Content-Length` (Chrome ignores `Connection: close`; port mode hung forever).

**Measured** (`cargo test -p surf-browser --test launch`, 5 tests, ~1.5 s): no listening sockets on the browser pid (`lsof`), temp profile gone after close, `navigator.webdriver=false` + 87 px chrome headed, `true` without the switch, port mode listens on loopback only. fmt / clippy `-D warnings` / `cargo test --workspace` green.

**Deferred** (TASKS.md): Windows io-pipes path unexercised; Linux `virtual: true` not run on this Mac (CI's `xvfb-run` sets `DISPLAY`); `surf doctor` should move to `discovery::find_chrome(None)`; `surf install` layout; step-10 webdriver gate.
