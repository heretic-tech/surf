# Decisions

Recorded so they are not re-litigated. Each entry: the decision, why, and
what it rules out. Change one only with a new entry that supersedes it.

## 1. Bytecode stack VM, no Cranelift JIT

Scripts are IO-bound: a script's own compute is microseconds between CDP
round-trips of milliseconds. A bytecode VM gives < 5 ms cold start, a tiny
binary, trivial debuggability, and runs on `wasm32` later. A JIT would add
build complexity and startup cost for no measurable gain. Rules out:
Cranelift, LLVM, tree-walking (too slow for `parallel for` over thousands of
items and awkward for async).

## 2. Hand-written lexer (INDENT/DEDENT) + recursive-descent / Pratt parser, ariadne diagnostics

Indentation-sensitive grammars need a lexer that tracks a stack of
indentation levels and emits synthetic tokens; parser combinators (nom,
chumsky) make that awkward and produce poor error messages. A hand-written
lexer and recursive-descent parser with a Pratt expression loop is ~2 k
lines, fully controllable, and lets `ariadne` render precise, labelled
diagnostics. Rules out: nom, chumsky, pest, lalrpop.

## 3. serde_json first (RawValue passthrough); simd-json only with a profile

CDP payloads are small except screenshots/DOM snapshots, which are
base64/`RawValue` passthrough anyway. `serde_json` with `raw_value` avoids
re-serialising untouched subtrees. `simd-json` is adopted only if a profile
shows JSON parsing on the hot path. Rules out: premature simd-json,
custom parsers.

## 4. No `stealth:` flag; quiet is the only mode

CDP is IPC; nothing in the page can see the protocol. What is detected is the
*side effects* of how tools use it: `--enable-automation`,
`Runtime.enable`, main-world injection, heavy domain enables, headless
rendering gaps. A tool with no reason to produce those side effects simply
does not. A `stealth:` flag would imply a default that is noisy and a mode
that is "evasion"; neither is true. Rules out: any flag, env var or option
that toggles quietness; "stealth" vocabulary in code and docs.

## 5. tokio current_thread + LocalSet; VM values `Rc`; surf-cdp `Send` for future sharding

One thread runs thousands of in-flight CDP calls comfortably; `Rc`/`RefCell`
values make the VM simpler and faster than `Arc`/`Mutex`. `surf-cdp` stays
`Send + Sync` (tokio::sync internally) so a thread-per-core runtime can
later own one connection set per thread without touching the protocol
layer. Rules out: multi-threaded tokio in v0.1, `Arc<Mutex<Value>>`.

## 6. Indentation syntax; implicit page rules

The 90 % case is one browser, one tab. Blocks by indentation, no braces,
no `await`, no `let`; `page` is implicit; bare actions resolve to the sole
page; with several pages the error names them and shows the fix
(`page(2).click(…)`). `page(n)` / `page("name")` auto-create. Every action
auto-waits. Rules out: explicit browser/page construction, promise-style
APIs, brace syntax.

## 7. Apostate is a later optional `engine: apostate` + `persona:`; core never depends on it

Apostate (a Chromium fork) can provide per-process fingerprint personas.
Surf must work with stock Chrome; Apostate support is additive
(`engine: apostate`, `persona:` on a browser or page) and lives behind the
same `Page::rebind` mechanism. In v0.1 `engine: apostate` yields a clear
"not yet" error. Rules out: any Apostate-only code path in core crates, any
build-time dependency on `~/apostate`.

## 8. `pool:` = remote websocket provider in v0.1; wasm/edge build later; surf-syntax/surf-vm stay wasm32-clean

`pool: "wss://…"` lets the same script run against a remote browser
provider with no code change. A wasm/edge build of the runtime (language +
VM in the browser or on an edge worker, CDP over websocket) is planned, so
`surf-syntax` and `surf-vm` carry no tokio / `std::process` / `std::fs`
dependency and are built for `wasm32-unknown-unknown` in CI. Rules out: IO
in the VM, platform-specific code below `surf-cdp`.

## 9. Reactive DOM via isolated-world MutationObserver + `Runtime.addBinding` / `Runtime.bindingCalled`, never `DOM.enable`

`DOM.enable` makes Chrome push the whole DOM tree and keep it in sync — a
heavy, observable side effect. A `MutationObserver` installed in an
isolated world (random helper names, no main-world access) calling a
binding registered with `Runtime.addBinding{executionContextId}` fires
`Runtime.bindingCalled` **without** `Runtime.enable`. This is verified by an
e2e test. Rules out: `DOM.enable`, `DOM.getDocument`-based polling,
`Runtime.enable`.

Verified (step 7, Google Chrome 154.0.8037.95, macOS arm64): the full
stack — `on element_appears("#appeared"):` in a script, observer installed
in a fresh isolated world after `goto`, element added by the page 800 ms
later — delivers `Runtime.bindingCalled` and runs the handler
(`tests/e2e/scripts/handler.surf`, plus
`surf-browser/tests/pages.rs::binding_called_fires_without_runtime_enable`
at the protocol level). The 50 ms isolated-world poll contemplated as a
fallback was not needed and is not implemented.

## 10. Page identity decoupled from the CDP target (rebind)

A script's `page` value is `Rc<PageInner>`; the CDP backing
`{session, target_id, browser_context_id, frame_id}` can be swapped by
`Page::rebind(new_backing, Migration{cookies, storage, url})`. One
mechanism serves supervisor restarts, `shift_proxy()`, and later per-page
personas (one process per persona; a logical `Browser` may own several OS
processes). Rules out: exposing target ids to scripts, re-creating script
values on restart.

## 11. Codegen typed CDP structs from vendored protocol JSON for an allow-list of domains; everything else via `call_raw`

`protocol/browser_protocol.json` + `js_protocol.json` are vendored (pinned
in `protocol/VERSION`). Typed `Command` structs are generated only for the
domains Surf uses (`Target`, `Page`, `Runtime` minus `enable`, `Input`,
`DOM` getContentQuads/scrollIntoViewIfNeeded, `Network`, `Fetch`, `Storage`,
`Emulation`, `Browser`), keeping compile time and binary size small; the
rest is `Session::call_raw`. Rules out: a full-protocol crate dependency,
hand-maintained structs for hundreds of methods.

## 12. One permitted `--disable-*`: `--disable-blink-features=AutomationControlled`, on launched browsers only

Quiet rule 2 assumed `navigator.webdriver` comes only from
`--enable-automation`. Measured otherwise (macOS arm64, Google Chrome
154.0.8037.95 stable, Chrome for Testing 133.0.6943.141 and 152.0.7977.54):

* control, no CDP at all: `chrome --headless --user-data-dir=… --dump-dom
  'data:text/html,…navigator.webdriver…'` → `wd=false`;
* same command plus `--remote-debugging-port=0` → `wd=true`;
* same command plus `--enable-automation` → `wd=true`;
* through `surf_browser::launch` with the quiet set only (pipe **or** port,
  headed **or** headless), the value read via `Target.createTarget` +
  `Target.getTargets` titles — no session attached to the page, no
  `Runtime.*` — all four combinations → `navigator.webdriver === true`.
  The automation infobar is absent in every case (`outerHeight −
  innerHeight` = 87 px at 1280×800 headed).

Chromium does this on purpose: a configured `--remote-debugging-pipe` /
`--remote-debugging-port` enables Blink's `AutomationControlled` runtime
feature (https://issues.chromium.org/issues/40746300). It is a side effect
Chrome imposes purely because a debugger transport exists, and the
feature's only observable effect is `navigator.webdriver`. Surf therefore
passes exactly one extra switch on every browser it **launches**:
`--disable-blink-features=AutomationControlled`. It is the minimal
correction of that side effect, not a stealth mode (Decision 4 stands: no
flag toggles it). It is never added when attaching to a browser Surf did
not launch (`cdp: "ws://…"`, `pool:`). Still forbidden:
`--enable-automation` and every other `--disable-*` the user did not write.
`crates/surf-browser/tests/launch.rs::webdriver_is_true_without_automationcontrolled_switch`
launches without the switch and asserts `true`, so the reason cannot be
"cleaned up" away. Supersedes the "never
`--disable-blink-features=AutomationControlled`" wording in rule 2.
