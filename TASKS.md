# Backlog

Deferred work, in rough priority order. Add an entry when you defer
something; link the PR / commit that picks it up. The first two sections
are the roadmap; their seams are drawn in `docs/architecture.md`
("Roadmap seams") and the rule in `AGENTS.md` is to build against the
seam, not beside it.

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
      frame; `Page.frameAttached` tracking. Today every action resolves in
      the main frame's world (`World::create(session, frame_id)` already
      takes any frame id, so child-frame worlds are a registry away).
- [ ] surf-browser `fill()` sets text through select-all +
      `Input.insertText`; `<input type=date|number|range|color|file>` need
      value assignment / `DOM.setFileInputFiles` instead (Playwright-style).
- [ ] surf-browser `text=` resolver walks light DOM only (no shadow roots);
      CSS selectors likewise do not pierce shadow DOM.
- [x] surf-browser `Page.setLifecycleEventsEnabled` is on for every page
      (needed to await `load` / `networkIdle`); it is a DevTools-only
      stream, not page-observable — noted in `docs/quiet-cdp.md` §2/§8.
- [ ] surf-browser: `Element` handles are released with a spawned
      `Runtime.releaseObject` on drop; a page that never navigates and
      resolves millions of elements still accumulates nothing, but a
      `Runtime.releaseObjectGroup("surf")` sweep on navigation would be
      cheaper than per-handle releases if a profile shows it.
- [ ] surf-browser: `pdf()` is headless-only (Chrome refuses
      `Page.printToPDF` headed); surface a clear message in the runtime.

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

## Runtime (step 7 / 8 / 9 deferrals)
- [ ] Handlers declared after a page already has its observer (REPL, or
      an `on …:` reached from a body) are installed on a spawned task, so
      the very next action may race them; top-level handlers are hoisted
      and awaited (`Runtime::instrument`) and never race. A page whose
      observer already runs does not pick up later-declared handlers at
      all (`observed` is per page) — restart the observer with the new
      handler set if the REPL needs it.
- [ ] `Page::mark_action` keeps two broadcast receivers armed between an
      action and the next `wait_navigation()` / action; a script that
      never calls `wait_navigation()` after a `click` holds them until the
      next action, and more than `EVENT_CHANNEL_CAPACITY` (1024) lifecycle
      events in between would log a lag warning. Harmless; drop the armed
      state on a timer if it ever shows up in logs.
- [ ] `wait_download()` matches downloads by the page's main `frameId`; a
      download started from an iframe is not attributed to the page. Track
      child frame ids when the frames API lands.
- [ ] `intercept` cannot see or rewrite response bodies (`Fetch.requestPaused`
      at the response stage, `Fetch.getResponseBody`, `fulfil` after the
      real response); only request-stage decisions exist today.
- [ ] `on response` `body()` fails for responses Chrome evicts from its
      buffer before `loadingFinished` is observed (large streams, or when
      the handler runs late); the error names the request. A
      `Network.setDataSizeLimits`-style knob is not exposed.
- [ ] `block([...])` patterns use Chrome's own `*` matcher (unanchored
      substring glob) while hooks / `wait_url` use Surf's anchored glob;
      documented, but a `re:` pattern for `block` would need a Fetch-based
      implementation.
- [ ] `h.cancel()` on a task that is itself waiting in a `parallel for`
      cancels the items (drop guard), but a task's *spawned* children are
      not cancelled with it (no parent/child tree beyond supervisors).
      Decide whether `spawn` inside a task should be structured.
- [ ] A joined task's error is a copy (`tasks::copy_error`): message, span,
      selector and CDP method survive, the original is the cause, but
      `is_no_browser` (CLI exit 3) only inspects the direct cause — a
      "no Chrome" failure inside a spawned task exits 1, not 3.
- [ ] Supervisor restarts rebind the child's page `SameContext` (cookies /
      storage intact, tab at `about:blank`); if the rebind fails the child
      gets a brand-new page. There is no per-child restart budget —
      `max_restarts` counts every restart of every child in the window.
- [ ] `broadcast` from the main body reaches every live task, including
      supervised ones; from a supervised task it stays inside the tree.
      There is no way to address a supervisor tree from outside it.
- [ ] Private task pages take registry indices (`page.index` inside a task
      is its private page's index); after the tasks end those indices are
      closed, so `page(n)` with a small `n` from the main body may hit
      `page(n) is closed`. Bare actions in the main body are unaffected
      (private pages are ignored by the sole-page rule).
- [ ] The proxy stub in `surf-testserver` maps every origin host to
      loopback and speaks only enough HTTP/1.1 for the fixture (one request
      per connection, `Connection: close`); `CONNECT` is tunnelled. Proxy
      *authentication* (407) is still untested — see the surf-browser entry
      above.
- [ ] `surf repl`: top-level variables do not persist between lines (each
      chunk compiles to its own `<main>` whose locals die with it). Needs a
      VM hook for "top-level assignments become host globals" (or the REPL
      re-declaring them through `Host::resolve_global`).
- [ ] `surf install` shells out to `curl` and `unzip` (`tar` on Windows).
      Replace with an in-process HTTPS client only if a dependency-free one
      is acceptable (rustls is already pulled in by tokio-tungstenite).
      Also: pin to a requested version (`surf install 131`), prune old
      versions, print the discovery order.
- [x] `examples/two-tabs.surf` typed into `input[name=q]` on example.com,
      which has no such input (IANA's 2025 redesign). Step 11 moved it to
      `www.wikipedia.org` / `input[name=search]` and verified every
      example in `examples/` runs against the live web.
- [ ] The `print` sink is `println!` (line-buffered stdout); `emit` of very
      large documents should go through a `BufWriter` with an explicit
      flush before exit. Also: `surf run … | head` panics with `failed
      printing to stdout: Broken pipe` once the reader goes away (seen in
      step 10) — handle `EPIPE` by exiting quietly.
- [ ] `page_method` temporarily swaps the page timeout for `goto(timeout:)`
      — racy if two tasks share a page (task 8 gives tasks private pages,
      but `page(n)` is shared). Thread the timeout through
      `Page::goto` instead.

## Tooling
- [ ] LSP server (`surf lsp`) + VS Code extension with a TextMate grammar.
- [x] `surf doctor` (step 7): discovery origin + version, display, Xvfb on
      Linux, timed headless pipe launch, `Browser.getVersion` round trip,
      the exact flag list, shutdown ladder timing. Step 10 added the
      `transport:` line (spawn → first `Browser.getVersion`) and
      `--detector` (the local detector page, which covers the
      `navigator.webdriver` check and an isolated-world eval).
      The path-only `surf_browser::find_chrome()` wrapper is now unused by
      the CLI; delete it when nothing else needs it.
- [x] `surf install` (step 7; see the deferral above for its limits).
- [x] Detection gate (step 10): `tools/detector/index.html` asserts
      `navigator.webdriver === false` (and the rest of `docs/quiet-cdp.md`
      §3) through the whole stack, headless and headed, in the e2e suite;
      `tests/launch.rs::webdriver_is_true_without_automationcontrolled_switch`
      stays as the control (Decision 12). If a future Chrome stops setting
      the flag for a debugger pipe, that control test fails and the switch
      can be reconsidered.
- [x] `surf repl` (step 7; variables do not persist — see above).
- [x] Detection + perf gates (step 10): `tests/perf.rs` behind
      `cargo test --release --test perf` (noop cold start 1.8 ms median,
      idle RSS 6.1 MB, start → first CDP frame 39–47 ms — all under budget,
      recorded in `docs/quiet-cdp.md` §5); the port-exposure gate and the
      detector scripts run in the e2e suite. CI runs the perf job on both
      runners with `continue-on-error: true` (reported, not blocking);
      make it blocking once the runners' numbers are known — they will
      differ from the Mac's and may need their own budgets.
- [ ] Detector follow-ups (step 10): the classic `Error.stack`-accessor
      leak no longer discriminates on Chrome 154 (the `prepareStackTrace`
      variant does) — re-measure both with each Chrome major and drop the
      classic one when no supported Chrome leaks there. `tools/detector/
      control.py` needs `websocket-client`; a Rust control (a tiny
      websocket client that sends `Runtime.enable`, bypassing
      `FORBIDDEN_METHODS`) would let CI prove the detector is not vacuous.
      Headless runs keep `HeadlessChrome` in the UA (informational; Surf
      does not rewrite it). rebrowser's `exposeFunctionLeak` is
      unexercisable (no `exposeFunction`); its three main-world traps stay
      untriggered from the isolated world — re-run `tools/live-detectors.surf`
      before quoting any live result, and again when the opt-in main-world
      eval (above) lands.

## Release / docs (step 11)
- [x] Move the repo to `heretic-tech/surf` (2026-10-05). `repository` in
      `Cargo.toml`, the two README badges and the `curl | sh` / `irm | iex`
      one-liners point at `heretic-tech/surf`; `dist generate` left
      `release.yml` unchanged (the URL is read from `repository` at build
      time); `origin` is `https://github.com/heretic-tech/surf.git`.
- [x] History: the personal `0xchasercat` GitHub account was suspended
      (Actions never created a run for any repo there — `total_count: 0`
      after four pushes plus the `v0.1.0` tag, with Actions enabled and both
      workflows `active`). The repo moved to `heretic-tech/surf` on
      2026-10-05, where Actions runs (first `ci` run on push:
      `actions/runs/37255565394`). `0xchasercat/surf` is a stale private
      copy carrying the old `repository` URL and the old `v0.1.0` tag
      (on `0009cf8`): delete it when/if that account is restored. The local
      clone keeps it as remote `suspended` with push disabled.
- [x] `.github/workflows/release.yml` ran for the first time on
      2026-10-05 for tag `v0.1.0` (annotated, on `7458171`, the commit that
      moved `repository` to `heretic-tech/surf`):
      https://github.com/heretic-tech/surf/actions/runs/37255902959 —
      green on the first attempt, 7 m 41 s. All five targets built
      (`aarch64-unknown-linux-gnu` 1 m 30 s, `x86_64-unknown-linux-gnu`
      1 m 34 s, `aarch64-apple-darwin` 1 m 41 s, `x86_64-pc-windows-msvc`
      2 m 44 s, `x86_64-apple-darwin` 6 m 08 s), then
      `build-global-artifacts`, `host`, `announce`. Release
      https://github.com/heretic-tech/surf/releases/tag/v0.1.0 carries
      `surf-cli-aarch64-apple-darwin.tar.xz` (2,008,012 B),
      `surf-cli-x86_64-apple-darwin.tar.xz` (2,309,000 B),
      `surf-cli-x86_64-unknown-linux-gnu.tar.xz` (2,367,848 B),
      `surf-cli-aarch64-unknown-linux-gnu.tar.xz` (2,054,024 B),
      `surf-cli-x86_64-pc-windows-msvc.zip` (3,738,087 B), their
      `.sha256` files, `surf-cli-installer.sh` (54,052 B — 5 mentions of
      `heretic-tech`, 0 of the old account), `surf-cli-installer.ps1`
      (22,260 B), `sha256.sum`, `source.tar.gz` (562,495 B),
      `dist-manifest.json`. Not yet done on the CI archives: unpack one
      and run `surf doctor` (only the local `dist build` archive was
      exercised that way, see the step-11 notes in git history). Separate
      from the release: `ci.yml` fails on heretic-tech (`test` jobs,
      `crates/surf-browser/tests/launch.rs`, 4 tests): Linux — the
      `setup-chrome` Chromium aborts with the SUID-sandbox error
      (`chrome-sandbox` not root/4755 → needs `--no-sandbox` in CI only,
      or a differently packaged Chrome); macOS — `Page.getFrameTree:
      timed out`. Track as its own item.
- [ ] `cargo publish` (crates.io) is not part of the pipeline; the
      procedure in `AGENTS.md` lists the order. `cargo install surf-cli`
      in the README is aspirational until the first publish.
- [ ] `docs/comparison.md`: chromedp and chromiumoxide rows are from
      source reading, not measurement (no Go toolchain / no fixture
      project). Add a `bench/` directory with the Node scripts and a Go /
      Rust equivalent so the table can be regenerated by one command;
      today the method is prose.
- [ ] Surf's launch → title → close round trip (488 ms) trails Puppeteer
      (453 ms) by the shutdown ladder's profile-removal wait (≈ 96 ms,
      `docs/quiet-cdp.md` §5). Removing the temp profile asynchronously
      after `Browser.close` returns would close that gap; make sure a
      `profile:` dir is never touched.
- [ ] The `perf` CI job is `continue-on-error: true` (budgets set on this
      Mac); make it blocking once the runners' numbers are known.
- [ ] `docs/language.md` §0 cheatsheet is hand-maintained; a `surf
      help <action>` or a generated list from `methods.rs::PAGE_METHODS`
      would keep it from drifting.

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
