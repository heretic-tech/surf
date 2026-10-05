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

Exercised (step 8): `fresh: true` retries (`RebindTarget::FreshContext`),
`shift_proxy()` (`RebindTarget::Proxy`), and supervisor restarts
(`RebindTarget::SameContext`) all go through `Browser::rebind_page_to` and
`Runtime::rebind`, which re-installs every handler observer on the new
session — `tests/e2e/scripts/handler-rebind.surf` shows `on
element_appears` firing before and after a `shift_proxy()` on the same
`page` value.

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

## 13. Persona language and process-per-persona key

Apostate's persona is the browser **process's** compositor: one persona
per OS process, none per browser context. So a persona is a process
(Decision 7) and the seam is `Page::rebind` (Decision 10). Settled before
any code so that the parser, the browser crate and the runtime build to
one spec (v0.3). The language:

```
browser:
    engine: apostate          # chrome (default) | apostate
    persona: 42               # seed shorthand
    persona: "windows"        # platform shorthand (apostate draws the seed)
    persona:                  # full form
        seed: 42              # int, or "host" (then no other key is allowed)
        platform: windows     # windows | macos | linux
        locale: "en-US"       # optional; else geoip (when a proxy is set) else host
        timezone: "Europe/Berlin"
        screen: "1920x1080"   # optional; never derived from size:
    personas: [42, {seed: 7, platform: macos}]   # rotation list for shift_persona()
    geoip: true               # default true iff a proxy is configured and engine is apostate
    max_processes: 8          # process-per-persona cap
p = browser.new_page(persona: {seed: 7, platform: "macos"}, proxy: "http://…")
page.set_persona(seed: 9)     # hot-swap: rebind to the process for that persona; cookies, storage and URL migrate; page reloads
page.set_proxy("http://…")    # hot-swap the proxy (new browser context; same process unless geoip changes the resolved persona)
shift_proxy() / shift_persona()   # round-robin over proxies: / personas:
page.persona()                # {engine, seed, platform, locale, timezone, screen}; persona(explain: true) adds apostate's --fingerprint-explain text
task fetch(url):
    persona: "windows"        # the task's private page is created with it
    retry: 3
    on_fail: shift_persona()
```

Semantics:

* `persona:` with `engine: chrome` is an error at first use, never
  ignored. Apostate's rule is "serve what the persona claims"; a persona
  silently dropped on stock Chrome would claim one machine and serve
  another, which is exactly what a detector looks for.
* A `ResolvedPersona` — seed, platform, locale, timezone, screen, *after*
  geoip (Decision 14) — together with the engine is the `ProcessKey`. Two
  pages with the same seed behind proxies that resolve to different
  timezones get different processes.
* The default process (slot 0) carries the `browser:` persona.
  `max_processes` caps the slots; a process with zero pages is retired
  when a new key needs a slot at the cap.
* On stock Chrome, per-page proxies keep using
  `Target.createBrowserContext{proxyServer}` in one process: no persona,
  no extra process.
* Hot-swaps (`set_persona`, `set_proxy`, `shift_persona()`,
  `shift_proxy()`) are the single `Page::rebind_to` path — the same code
  as supervisor restarts — so handlers are re-installed by the existing
  post-rebind hook (`Runtime::rebind`) and nothing above `surf-browser`
  learns a second mechanism.
* Same seed ⇒ same machine every launch; no seed ⇒ Apostate draws one.
  `seed: "host"` refuses per-field overrides (the launch exits non-zero),
  so Surf refuses before launching. `--fingerprint*` switches are
  validated against Apostate's own switch list, because Chromium ignores
  a misspelled switch silently. `screen` is never derived from `size:`
  (window size and claimed display are different claims).
* Fonts for a Windows / macOS persona must be installed on the host
  (`apostate fonts install windows`); Surf hints, never installs.

Rules out: per-browser-context personas; a persona that is ignored on
`engine: chrome`; deriving `screen` from `size:`; a second rebind
mechanism for personas; Apostate code in core crates (Decision 7 stands).

## 14. geoip for personas

A persona claiming `Europe/Berlin` behind a proxy that exits in Virginia
is a contradiction any detector can see; Apostate's own wrapper resolves
locale and timezone from the proxy's exit IP when they are not given.
Surf does the same: with `engine: apostate`, a proxy configured and
`locale` / `timezone` unset, both are resolved from the proxy's exit IP
through the plain-HTTP endpoints Apostate's wrapper uses —
`http://ip-api.com/json/`, `http://ipinfo.io/json`, `http://ipwho.is/`,
`http://ifconfig.co/json`, two attempts each, in that order, through the
proxy itself — before the process for that persona launches. `geoip:
true|false` on `browser:` overrides the default (`true` iff a proxy is
configured and the engine is apostate; it is never on for stock Chrome,
which has no persona to correct). Explicit `locale:` / `timezone:` win;
with no proxy the host's values are used. The resolved values are part of
the `ProcessKey` (Decision 13), so `set_proxy` to an exit in another
timezone moves the page to another process. When every endpoint fails
the launch fails with the endpoints tried — never a silent fall-back to
the host's values ("never claim what you do not serve"). The suite stubs
the endpoints in `surf-testserver` and never calls out. Rules out:
geoip on stock Chrome, silent host fall-back, HTTPS-only endpoints the
test proxy stub cannot serve.

## 15. Apostate test policy

Tests that need Apostate follow the Chrome rule in `AGENTS.md`:
`surf_browser::discovery::apostate_or_skip(name)` — `SURF_APOSTATE=/path`
overrides discovery (set-but-missing is an error, not a fallback);
otherwise the cache (`APOSTATE_CACHE_DIR`, `~/Library/Caches/apostate`,
`$XDG_CACHE_HOME/apostate` / `~/.cache/apostate`,
`%LOCALAPPDATA%\apostate\cache`; `<version>/<platform-dir>/`, newest
version first). When nothing is found the test prints `skipping <name>:
no Apostate found …` with the locations tried and passes. They **must**
actually run on the dev Mac (`~/Library/Caches/apostate` holds
155.0.8059.31 and 152.0.7977.83) — never merge on a skip. CI runners
have no Apostate, so a CI skip is expected and is not evidence; the dev
Mac run is, and the report says so. `~/apostate` and the cache are never
modified. Rules out: vendoring Apostate, a CI job that downloads it,
merging on a skip.
