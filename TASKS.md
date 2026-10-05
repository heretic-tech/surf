# Backlog

Organised by increment (`v0.2` … `v0.5`), then `Later` (parked on
purpose) and `Notes` (closed entries and scaffold notes, kept as
evidence). Each increment is one release. Every open item lives in exactly
one increment; move it only with a line saying why. Keep an item's text
when you move or close it — the detail is hard-won. Tick what you close
(link the commit); add an entry for anything you defer, with the reason.
The seams the open items build on are drawn in `docs/architecture.md`
("Roadmap seams"); the rule in `AGENTS.md` is to build against the seam,
not beside it. The v0.3 persona design is settled in `DECISIONS.md`
#13–#15 and sketched in `docs/language.md` §12.

## v0.2 — polish

The deferrals of the v0.1 run, all additive. A new language feature or
action ships with an e2e script and a `docs/language.md` entry in the same
commit; the gates in `AGENTS.md` are part of done.

### Language / VM
- [x] Compound assignment (`+=`, `-=`, `*=`, `/=`, `%=`) on names, fields
      and index targets (`StmtKind::CompoundAssign`; receiver / index
      evaluated once via `Op::Dup` / `Op::Dup2`; fixture
      `compound_assign.surf`, snapshot `stmt_compound_assign`).
- [x] String methods as methods (`s.upper()`), and list / map methods
      likewise (`xs.len()`), next to the free-function forms that exist
      today. Already true since v0.1 (`stdlib::forward` makes every free
      function call the method); fixture `methods.surf` pins both forms to
      one implementation.
- [x] surf-syntax: a statement that starts with a list literal
      (`[1, 2].each(f)`) fails to parse. Root cause: after a *block lambda*
      (`f = fn(x):` + body) the postfix / infix loop kept going, so the
      next line's `[` indexed the lambda (and `(…)` called it, `-x`
      subtracted). A block lambda now ends its expression (snapshot
      `stmt_after_block_lambda`).
- [x] surf-vm: `spawn` of a non-global callee (`f = fn(): …; spawn f()`) →
      `Op::SpawnValue` → `Host::spawn_value(vm, callee, args)` (default
      errors). The runtime wiring is the item under Runtime; fixture
      `spawn_value.surf`.
- [x] surf-vm: reads of a function-scoped variable before its first
      assignment are `variable `x` used before assignment` (an unset slot
      marker, `Slot::Unset`; names in `Chunk::local_names`). A closure that
      captures an unset local boxes `nil` (documented). Fixture
      `unassigned.surf`, `docs/language.md` § 3.1. **Runtime batch: run the
      e2e suite** — a script that read a branch-only variable as `nil`
      now errors and needs `x = nil` first.
- [x] Raw strings `r"…"` (no escapes, no interpolation, single line, no
      `"` inside). `docs/language.md` § 1.6; fixture `raw_strings.surf`.
- [x] surf-syntax: block lambdas inside brackets. Layout rule (lexer): a
      line inside brackets ending in `):` followed by a deeper-indented
      line opens a layout block (`NEWLINE INDENT … DEDENT`) that stays live
      at exactly that bracket depth, and closes on the first line indented
      at or below the header or when the holding bracket closes on the
      block's last line (`NEWLINE DEDENT` are emitted before the `)`).
      Only `):` opens a block, so `key:` map entries and keyword arguments
      are untouched; all 41 `.surf` files in the repo lex identically.
      Fixture `block_lambda_args.surf`, snapshot
      `expr_block_lambda_in_brackets`.
- [x] surf-vm: `now()` / `random()` route through `Host::now() -> f64` /
      `Host::random_seed() -> u64` (defaults use `std::time`; the wasm host
      overrides). The RNG seed no longer mixes the thread-local's address,
      so a fixed host seed is reproducible across threads.
- [x] surf-vm: `CompileOptions { top_level_globals }` +
      `compile_with_options`: top-level assignments emit `Op::SetGlobal` →
      `Host::set_global(name, value)` (default errors) and reads go through
      `Host::resolve_global`. Test `top_level_globals_persist_across_programs`.
      The CLI REPL wiring is the item under Runtime.

### Browser
- [ ] surf-browser `fill()` sets text through select-all +
      `Input.insertText`; `<input type=date|number|range|color|file>` need
      value assignment / `DOM.setFileInputFiles` instead (Playwright-style).
      Add `set_files(sel, paths)` for file inputs.
- [ ] surf-browser `text=` resolver walks light DOM only (no shadow roots);
      CSS selectors likewise do not pierce shadow DOM.
- [ ] Frames API: `frame("name")` / iframe targeting with isolated worlds per
      frame; `Page.frameAttached` tracking. Today every action resolves in
      the main frame's world (`World::create(session, frame_id)` already
      takes any frame id, so child-frame worlds are a registry away).
- [ ] `wait_download()` matches downloads by the page's main `frameId`; a
      download started from an iframe is not attributed to the page. Track
      child frame ids when the frames API lands.
- [ ] `intercept` cannot see or rewrite response bodies (`Fetch.requestPaused`
      at the response stage, `Fetch.getResponseBody`, `fulfil` after the
      real response); only request-stage decisions exist today.
- [ ] `block([...])` patterns use Chrome's own `*` matcher (unanchored
      substring glob) while hooks / `wait_url` use Surf's anchored glob;
      documented, but a `re:` pattern for `block` would need a Fetch-based
      implementation.
- [ ] surf-browser: `Element` handles are released with a spawned
      `Runtime.releaseObject` on drop; a page that never navigates and
      resolves millions of elements still accumulates nothing, but a
      `Runtime.releaseObjectGroup("surf")` sweep on navigation would be
      cheaper than per-handle releases if a profile shows it.
- [ ] Surf's launch → title → close round trip (488 ms) trails Puppeteer
      (453 ms) by the shutdown ladder's profile-removal wait (≈ 96 ms,
      `docs/quiet-cdp.md` §5). Removing the temp profile asynchronously
      after `Browser.close` returns would close that gap; make sure a
      `profile:` dir is never touched.
- [ ] surf-browser launcher: port mode (`cdp: N`) connects through
      `surf_cdp::transport::tcp::connect`, which has no timeouts of its own;
      the launcher wraps each attempt in 3 s. Fold a timeout into `tcp` if
      attach mode (`cdp: "ws://…"`, step 7) needs one too.
- [ ] surf-browser: `pdf()` is headless-only (Chrome refuses
      `Page.printToPDF` headed); surface a clear message in the runtime.
- [ ] `Page::mark_action` keeps two broadcast receivers armed between an
      action and the next `wait_navigation()` / action; a script that
      never calls `wait_navigation()` after a `click` holds them until the
      next action, and more than `EVENT_CHANNEL_CAPACITY` (1024) lifecycle
      events in between would log a lag warning. Harmless; drop the armed
      state on a timer if it ever shows up in logs.

### Runtime
- [ ] Supervisor restarts rebind the child's page `SameContext` (cookies /
      storage intact, tab at `about:blank`); if the rebind fails the child
      gets a brand-new page. There is no per-child restart budget —
      `max_restarts` counts every restart of every child in the window.
      Add a per-child budget, and `fresh: true` on supervisors (restart
      into a fresh context, as task retries already can).
- [ ] `h.cancel()` on a task that is itself waiting in a `parallel for`
      cancels the items (drop guard), but a task's *spawned* children are
      not cancelled with it (no parent/child tree beyond supervisors).
      Decide whether `spawn` inside a task should be structured.
- [ ] Wire `Host::spawn_value(vm, callee: Value, args)` (Language / VM
      above; default errors) through the runtime's task machinery so a
      closure value spawns like a named task (handle, `cancel`, join,
      private page). Until then `spawn f()` on a local closure is the
      runtime error `spawn of a fn value is not supported by this host`.
- [ ] A joined task's error is a copy (`tasks::copy_error`): message, span,
      selector and CDP method survive, the original is the cause, but
      `is_no_browser` (CLI exit 3) only inspects the direct cause — a
      "no Chrome" failure inside a spawned task exits 1, not 3.
- [ ] Private task pages take registry indices (`page.index` inside a task
      is its private page's index); after the tasks end those indices are
      closed, so `page(n)` with a small `n` from the main body may hit
      `page(n) is closed`. Bare actions in the main body are unaffected
      (private pages are ignored by the sole-page rule).
- [ ] `broadcast` from the main body reaches every live task, including
      supervised ones; from a supervised task it stays inside the tree.
      There is no way to address a supervisor tree from outside it — give
      the supervisor reference a `send`.
- [ ] The `print` sink is `println!` (line-buffered stdout); `emit` of very
      large documents should go through a `BufWriter` with an explicit
      flush before exit. Also: `surf run … | head` panics with `failed
      printing to stdout: Broken pipe` once the reader goes away (seen in
      step 10) — handle `EPIPE` by exiting quietly.
- [ ] `page_method` temporarily swaps the page timeout for `goto(timeout:)`
      — racy if two tasks share a page (task 8 gives tasks private pages,
      but `page(n)` is shared). Thread the timeout through
      `Page::goto` instead.
- [ ] Handlers declared after a page already has its observer (REPL, or
      an `on …:` reached from a body) are installed on a spawned task, so
      the very next action may race them; top-level handlers are hoisted
      and awaited (`Runtime::instrument`) and never race. A page whose
      observer already runs does not pick up later-declared handlers at
      all (`observed` is per page) — restart the observer with the new
      handler set if the REPL needs it.
- [ ] `surf repl`: top-level variables do not persist between lines (each
      chunk compiles to its own `<main>` whose locals die with it). The VM
      hook landed (Language / VM above): compile each line with
      `surf_vm::compile_with_options(name, Some(src), &program,
      CompileOptions { top_level_globals: true })`, implement
      `Host::set_global(name, value)` on the runtime host (a
      `RefCell<IndexMap>` consulted first by `resolve_global`), and keep
      the `Globals` / host across lines.
- [ ] `docs/language.md` §0 cheatsheet is hand-maintained; a `surf
      help <action>` or a generated list from `methods.rs::PAGE_METHODS`
      would keep it from drifting. Ship `surf help` and a drift test that
      diffs the cheatsheet against `PAGE_METHODS`.
- [ ] surf-syntax: the known-key lists for `browser:` / task / supervisor /
      `parallel for` props and handler events live in `parser.rs`
      (`BROWSER_PROPS`, …). When the runtime adds a key, update the list and
      `docs/language.md` together (consider generating one from the other —
      the drift test above can cover it). v0.3 adds `persona`, `personas`,
      `geoip`, `max_processes`.

### Platform / CI
- [ ] Windows pipe transport: `transport::pipe::windows` (CreatePipe +
      SetHandleInformation, `--remote-debugging-io-pipes=<read>,<write>`) is
      written and compiles behind `#[cfg(windows)]` but has never run — needs
      a Windows CI job (`cargo test -p surf-cdp` there, plus the Chrome pipe
      e2e). Parent ends are `tokio::fs::File` (blocking pool); revisit if
      latency matters.
- [ ] surf-browser launcher: Windows path (`--remote-debugging-io-pipes`,
      no `SIGTERM` → straight to `TerminateProcess`) is written but has
      never run; needs the Windows CI job above. Expect fixes.
- [ ] surf-browser launcher: Linux `display.rs` (Xvfb) compiles only on
      Linux and was not exercised on this Mac; CI's `xvfb-run` job sets
      `DISPLAY`, so `virtual: true` is a no-op there. Add a Linux job that
      unsets `DISPLAY` and runs a `virtual: true` launch.
- [ ] The `perf` CI job is `continue-on-error: true` (budgets set on this
      Mac); make it blocking once the runners' numbers are known. First
      green run (37257222508): ubuntu-latest 1.33 ms / 8.4 MB / 31.7 ms,
      macos-latest 2.42 ms / 5.9 MB / 100.3 ms (noop cold start / idle RSS
      / first CDP frame) — all under budget; a few more runs, then flip,
      with per-runner budgets.
- [ ] `surf install` shells out to `curl` and `unzip` (`tar` on Windows).
      Replace with an in-process HTTPS client only if a dependency-free one
      is acceptable (rustls is already pulled in by tokio-tungstenite).
      Also: pin to a requested version (`surf install 131`), prune old
      versions, print the discovery order (`surf install --list`).
- [ ] `docs/comparison.md`: chromedp and chromiumoxide rows are from
      source reading, not measurement (no Go toolchain / no fixture
      project). Add a `bench/` directory with the Node scripts and a Go /
      Rust equivalent so the table can be regenerated by one command;
      today the method is prose.

### Release
- [ ] Release 0.2.0: `AGENTS.md` "Release procedure" steps 1–5 (step 6,
      crates.io, waits for v0.4), plus a `CHANGELOG.md` (one section per
      release, v0.1.0 back-filled from the tag) maintained from here on.
      Also still owed from v0.1.0: unpack one of the CI-built archives and
      run its `surf doctor` (only the local `dist build` archive was
      exercised that way).

## v0.3 — engines and personas

Design: `DECISIONS.md` #13 (persona language, process-per-persona key),
#14 (geoip), #15 (Apostate test policy); syntax sketch in
`docs/language.md` §12. Apostate stays additive (Decision 7): core crates
never depend on it, stock Chrome keeps working, and `persona:` with
`engine: chrome` is an error at first use — never ignored.

- [ ] `engine: apostate` — launch the Apostate fork (discovery via
      `~/Library/Caches/apostate` / `SURF_APOSTATE`), clear "not yet" error
      until then. (Decision 7) Discovery walks
      `<cache>/<version>/<platform-dir>/` (`Chromium.app/Contents/MacOS/Chromium`
      on macos-arm64, `chrome` on linux-x64/arm64, `chrome.exe` on
      windows-x64; cache dir = `APOSTATE_CACHE_DIR` |
      `~/Library/Caches/apostate` | `$XDG_CACHE_HOME/apostate` or
      `~/.cache/apostate` | `%LOCALAPPDATA%\apostate\cache`), newest version
      first. The flag list stays in `LaunchConfig::args` (a new flag is a
      `DECISIONS.md` entry + `docs/quiet-cdp.md` §1); `--fingerprint*`
      switches are validated against Apostate's own list
      (`python/apostate/config.py` FINGERPRINT_SWITCHES — Chromium ignores a
      misspelled switch silently); `--fingerprint=host` with any per-field
      override is refused before launch (Apostate exits non-zero on it).
      Env: `GOOGLE_API_KEY=no GOOGLE_DEFAULT_CLIENT_ID=no
      GOOGLE_DEFAULT_CLIENT_SECRET=no` (else a "Google API keys are
      missing" infobar). Re-measure Decision 12 on Apostate (its docs claim
      `navigator.webdriver` stays false under its driver switches — measure
      before relying on it) and record the result in `DECISIONS.md`.
- [ ] `persona:` on `browser:` and per page — per-page fingerprints via
      process-per-persona + `Page::rebind`; a logical `Browser` owning several
      OS processes. (Decisions 7, 10, 13) In pieces:
  - [ ] `surf_browser::persona::{Persona, ResolvedPersona}`: `seed` (int,
        or `"host"` — then no other key is allowed), `platform`
        (windows | macos | linux), `locale`, `timezone`, `screen`
        (`"1920x1080"`; never derived from `size:`); shorthands `42` (seed)
        and `"windows"` (platform, Apostate draws the seed). Maps to
        `--fingerprint=`, `--fingerprint-platform=`, `--fingerprint-locale=`,
        `--fingerprint-timezone=`, `--fingerprint-screen-width=` /
        `-height=`. Same seed ⇒ same machine every launch.
  - [ ] geoip (Decision 14): with `engine: apostate`, a proxy configured
        and `locale` / `timezone` unset, resolve both from the proxy's exit
        IP through the plain-HTTP endpoints Apostate's wrapper uses
        (`http://ip-api.com/json/`, `http://ipinfo.io/json`,
        `http://ipwho.is/`, `http://ifconfig.co/json`; two attempts each,
        through the proxy). `geoip: false` turns it off. Stub the endpoints
        in `surf-testserver` so the suite never calls out.
  - [ ] Multi-process `Browser`: `ProcessKey = (engine, ResolvedPersona)`;
        slot 0 carries the `browser:` persona; `max_processes:` (default 8)
        caps the slots; a process with zero pages is retired when a new key
        needs a slot at the cap. On stock Chrome per-page proxies keep using
        `Target.createBrowserContext{proxyServer}` in one process.
  - [ ] `Page::rebind_to(ProcessKey, Migration)` — the single path for
        `set_persona`, `set_proxy`, `shift_persona()`, `shift_proxy()` and
        supervisor restarts; cookies, storage and URL migrate, the page
        reloads, handlers are re-installed by the existing post-rebind hook
        (`Runtime::rebind`).
  - [ ] Runtime + syntax: `engine: persona: personas: geoip: max_processes:`
        on `browser:` (parser known-key lists and `docs/language.md` §4
        together); `browser.new_page(persona:, proxy:)`;
        `page.set_persona(…)`, `page.set_proxy(…)`, `shift_persona()`,
        `page.persona()` / `persona(explain: true)` (adds Apostate's
        `--fingerprint-explain` text); `persona:` on `task` (the private
        page is created with it); `on_fail: shift_persona()`. e2e scripts
        for each, skipping per Decision 15.
  - [ ] Fonts: a Windows / macOS persona needs the host fonts installed
        (`apostate fonts install windows`); Surf hints (error text,
        `surf doctor`), never installs.
- [ ] Detector on Apostate: run `tools/detector/index.html` (headless and
      headed) and `tools/live-detectors.surf` on Apostate with a persona;
      record in `docs/quiet-cdp.md` §4 with the date and Apostate version.
      Tests skip with a printed message when no Apostate is found and must
      run on the dev Mac (Decision 15).
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
      eval (v0.4) lands.
- [ ] The proxy stub in `surf-testserver` maps every origin host to
      loopback and speaks only enough HTTP/1.1 for the fixture (one request
      per connection, `Connection: close`); `CONNECT` is tunnelled. Proxy
      *authentication* (407) is still untested. Grow it for `set_proxy`
      and the geoip stubs above.
- [ ] `docs/engines.md`: chrome vs apostate — discovery order, flags, env,
      personas, geoip, fonts, what the detector shows on each;
      `surf doctor` reports the engine.
- [ ] Release 0.3.0 (same procedure as 0.2.0; `CHANGELOG.md` entry).

## v0.4 — language and tooling

- [ ] `schedule:` blocks (cron-like: `schedule every 5m:` / `schedule at
      "09:00":`).
- [ ] Modules / imports (`use "./lib.surf"`), with a module cache and
      diagnostics spanning files.
- [ ] `surf fmt` — canonical formatter driven by the AST (spans preserved).
- [ ] LSP server (`surf lsp`) + VS Code extension with a TextMate grammar.
- [ ] `cargo publish` (crates.io) is not part of the pipeline; the
      procedure in `AGENTS.md` lists the order. `cargo install surf-cli`
      in the README is aspirational until the first publish.
- [ ] `surf help` polish: per-action pages with examples, `surf help
      <topic>` for config keys and handlers, generated from the same table
      as the v0.2 drift test.
- [ ] surf-syntax: `eval(fn(): …)` bodies are parsed as Surf today (the
      runtime slices `LambdaBody::Expr(e).span` from the source to get the
      verbatim JS). Decide whether to add a raw-JS lambda form
      (`js(…)`/backticks) for expressions Surf cannot parse (`=>`, `?.`).
      (Not in the increment map; parked here as the nearest fit — v0.2 raw
      strings cover the `eval("…")` half.)
- [ ] Main-world eval (opt-in, explicit): `addBinding` technique — install a
      binding in an isolated world, use a page-side trampoline so main-world
      code never sees `Runtime.enable`. Needs a design note on the observable
      side effects before implementation. (Not in the increment map; parked
      here as the nearest fit.)

## v0.5 — edge and scale

- [ ] WASM / edge target: build `surf-syntax` + `surf-vm` + a websocket-only
      runtime (`surf-runtime-ws`) for `wasm32-unknown-unknown`; CDP over
      `pool:` only. (Decision 8) Depends on `Host::now` / `random_seed`
      from v0.2.
- [ ] Thread-per-core sharding: multi-threaded runtime where each thread owns
      a set of browsers; `surf-cdp` is already `Send`. (Decision 5)
- [ ] CBOR pipe framing (`--remote-debugging-pipe=cbor`) behind a profile
      showing JSON on the hot path. (Decision 3)
- [ ] surf-cdp: the single driver task re-creates `Transport::recv` on every
      outgoing frame (cancel-safe by contract). If a profile shows this on the
      hot path, add a split read/write transport (two tasks) — the public
      `Transport` trait would gain an optional `split()`.

## Later

Parked on purpose, no increment. Pick one up only with a reason (a
profile, a user report) recorded next to it.

- surf-syntax: keyword-adjacent tokens with no space (`1if`, `x.in`) lex
  as number + keyword / error; fine for now, revisit with `surf fmt`.
- surf-vm: parameter defaults are limited to the first 64 parameters (a
  `u64` "missing" mask per frame); more parameters silently treat the
  default as supplied.
- surf-vm: `Op` is an enum of up to 16 bytes; pack to u32 words only if a
  profile ever shows dispatch cost — not worth it for IO-bound scripts.
- surf-cdp: `Session::events` channels are created lazily and dropped only
  when their last receiver is gone *and* an event arrives; add an explicit
  `unsubscribe`/sweep only if idle sessions pile up (parallel pools).
- A `Network.setDataSizeLimits`-style knob for the response-body buffer is
  not exposed.
- `on response` `body()` / `intercept` of responses Chrome evicts from its
  buffer before `loadingFinished` is observed (large streams, or when the
  handler runs late) fails with the request named; no fix planned.

## Notes

Closed entries and scaffold notes, kept as evidence. Do not delete; append
when a later step changes the picture.

### Closed
- [x] surf-browser `Page.setLifecycleEventsEnabled` is on for every page
      (needed to await `load` / `networkIdle`); it is a DevTools-only
      stream, not page-observable — noted in `docs/quiet-cdp.md` §2/§8.
- [x] `examples/two-tabs.surf` typed into `input[name=q]` on example.com,
      which has no such input (IANA's 2025 redesign). Step 11 moved it to
      `www.wikipedia.org` / `input[name=search]` and verified every
      example in `examples/` runs against the live web.
- [x] `surf doctor` (step 7): discovery origin + version, display, Xvfb on
      Linux, timed headless pipe launch, `Browser.getVersion` round trip,
      the exact flag list, shutdown ladder timing. Step 10 added the
      `transport:` line (spawn → first `Browser.getVersion`) and
      `--detector` (the local detector page, which covers the
      `navigator.webdriver` check and an isolated-world eval).
      The path-only `surf_browser::find_chrome()` wrapper is now unused by
      the CLI; delete it when nothing else needs it.
- [x] `surf install` (step 7; see the v0.2 Platform / CI entry for its
      limits).
- [x] Detection gate (step 10): `tools/detector/index.html` asserts
      `navigator.webdriver === false` (and the rest of `docs/quiet-cdp.md`
      §3) through the whole stack, headless and headed, in the e2e suite;
      `tests/launch.rs::webdriver_is_true_without_automationcontrolled_switch`
      stays as the control (Decision 12). If a future Chrome stops setting
      the flag for a debugger pipe, that control test fails and the switch
      can be reconsidered.
- [x] `surf repl` (step 7; variables do not persist — see v0.2 Runtime).
- [x] Detection + perf gates (step 10): `tests/perf.rs` behind
      `cargo test --release --test perf` (noop cold start 1.8 ms median,
      idle RSS 6.1 MB, start → first CDP frame 39–47 ms — all under budget,
      recorded in `docs/quiet-cdp.md` §5); the port-exposure gate and the
      detector scripts run in the e2e suite. CI runs the perf job on both
      runners with `continue-on-error: true` (reported, not blocking);
      making it blocking is a v0.2 Platform / CI item.
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
      exercised that way, see the step-11 notes in git history) — carried
      into the v0.2 Release item.
- [x] `ci.yml` green on heretic-tech
      (https://github.com/heretic-tech/surf/actions/runs/37257222508).
      The 4 `crates/surf-browser/tests/launch.rs` failures were all
      `browser-actions/setup-chrome`, whose "stable" is Chrome for
      Testing: Linux — non-setuid `chrome-sandbox` plus ubuntu-24.04's
      AppArmor userns restriction → SUID-sandbox abort at startup; macOS —
      the bundle is cached without its `.app` wrapper, so every child
      process dies at the Mach-port rendezvous (`bootstrap_look_up …
      Permission denied`) and renderer-side `Page.getFrameTree` never
      answers (reproduced locally by copying a CfT `Contents/` out of its
      `.app`; renaming the directory to `*.app` fixes it); and headed CfT
      shows its own permanent disclaimer infobar
      (`ChromeForTestingInfoBarDelegate`, 143 px of window chrome vs
      87 px on Google Chrome), which the no-infobar test cannot tell from
      the automation one. CI now pins `SURF_CHROME` to the runner images'
      stock Google Chrome stable (`/opt/google/chrome/chrome`,
      `/Applications/Google Chrome.app/…`); no Surf code or flag changed.

### Scaffold notes (step 1)
- The wasm32 build needs the rustup toolchain's `rustc`; on a machine where
  a non-rustup `cargo`/`rustc` shadows `~/.cargo/bin` (MacPorts here), run
  `RUSTC=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin/rustc
  $HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin/cargo build -p
  surf-syntax -p surf-vm --target wasm32-unknown-unknown`. CI uses rustup
  directly.
- `protocol/generated.rs` placeholder was replaced in step 2 by
  `crates/surf-cdp/build.rs` codegen (`$OUT_DIR/protocol.rs`, ~33 k lines
  for the 12-domain allow-list; adds ~3 s to a clean `surf-cdp` build).
