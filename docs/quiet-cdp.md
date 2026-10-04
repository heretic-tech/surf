# Quiet CDP: what Surf sends, what detectors see

Surf has no stealth mode (DECISIONS.md #4). This document is the evidence
that it does not need one: the exact command line, the exact protocol
traffic, a local replica of the public detector checks that runs in the
e2e suite, a control run showing the replica *does* flag a noisy client,
the public detectors' verdicts on a given day, the performance gates, and
the gaps we know about.

All numbers below: macOS 15 arm64 (Apple M4 Max), Google Chrome
154.0.8037.95 stable, Surf 0.1.0 release build, measured 2026-10-04.

## 1. Command line

`surf_browser::launch::LaunchConfig::args` is the only place flags are
decided. A launched browser gets, in this order and nothing else:

```
--remote-debugging-pipe                            # or --remote-debugging-port=N when the script says `cdp: N`
--user-data-dir=<temp dir | profile:>              # always; Chrome ≥ 136 ignores pipe/port on the default profile
--no-first-run
--no-default-browser-check
--disable-blink-features=AutomationControlled      # the one --disable-*; see DECISIONS.md #12
--window-size=W,H                                  # `size:`, default 1280,800
--headless                                         # only when headless
--proxy-server=scheme://host:port                  # only with `proxy:`; credentials stripped (answered via Fetch.authRequired)
<user flags verbatim>                              # `flags: [...]`
about:blank
```

Never `--enable-automation`, never any other `--disable-*` the user did not
write, no extension loading, no `--remote-allow-origins`. On Windows the
pipe flag is `--remote-debugging-io-pipes=<r>,<w>`. Nothing is added when
Surf attaches to a browser it did not launch (`cdp: "ws://…"`, `pool:`).

Why the one `--disable-*`: Chromium turns Blink's `AutomationControlled`
feature on whenever a debugger pipe or port is configured, and the only
observable effect of that feature is `navigator.webdriver === true`.
`crates/surf-browser/tests/launch.rs::webdriver_is_true_without_automationcontrolled_switch`
launches *without* the switch and asserts `true`, so the reason stays
measured.

`surf doctor` prints the flag list of the browser it launched.

## 2. Protocol traffic

Transport: two anonymous pipes, Chrome reads commands on fd 3 and writes
on fd 4, one UTF-8 JSON message + `\0` per frame. No TCP listener exists
unless the script says `cdp: 9222` — verified while a script holds a
browser open by `lsof -iTCP -sTCP:LISTEN -a -p <chrome pid>` and
`-p <surf pid>` (both empty;
`tests/e2e/main.rs::pipe_transport_opens_no_listening_port`).

Domains and when they are enabled:

| domain | when | owner |
|--------|------|-------|
| `Page.enable` + `Page.setLifecycleEventsEnabled{enabled:true}` | every page, for the lifetime of the page | the page (`DomainGuard`) |
| `Network.enable` | only while an `on request` / `on response` handler or a `block([...])` list exists on the page; `Network.disable` when the last one goes | `NetworkHooks` / the page's block guard |
| `Fetch.enable` | only while an `intercept(pattern):` block is active or a proxy challenge is still unanswered; `Fetch.disable` afterwards | the page's `FetchHub` |
| `Runtime.enable` | **never** — refused by `surf_cdp::FORBIDDEN_METHODS` in every build; no typed command is generated for it | — |
| `DOM.enable` | **never** — same | — |
| `Emulation.*`, `Storage.*`, `Input.*`, `DOM.getContentQuads`, `DOM.scrollIntoViewIfNeeded`, `Page.captureScreenshot`, `Browser.*`, `Target.*` | per call, no enable needed | — |

Execution contexts come from `Page.createIsolatedWorld{frameId, worldName:
<random>, grantUniveralAccess: true}`; everything a script evaluates runs
in that world through `Runtime.callFunctionOn{executionContextId}` /
`Runtime.evaluate{contextId}`. Reactive handlers use
`Runtime.addBinding{executionContextId}` → `Runtime.bindingCalled`, which
fires without `Runtime.enable` (DECISIONS.md #9). The selector resolver and
the MutationObserver live on the isolated world's global under random
names; the main world cannot see them (tested).

Every frame of the headed detector run (§3) with `--trace-cdp`, counted by
method — one `*.enable`, no `DOM.getDocument`, no `Network`/`Fetch`:

```
 42 Runtime.callFunctionOn        3 DOM.scrollIntoViewIfNeeded   1 Page.getFrameTree
 28 Runtime.releaseObject         3 DOM.getContentQuads          1 Page.enable
 26 Input.dispatchKeyEvent        1 Target.createTarget          1 Page.createIsolatedWorld
 12 Runtime.getProperties         1 Target.attachToTarget        1 Browser.getVersion
  4 Input.dispatchMouseEvent      1 Page.setLifecycleEventsEnabled  1 Browser.close
                                  1 Page.navigate
```

`crates/surf-browser/tests/pages.rs::quiet_contract_only_page_enable_is_sent`
asserts the same over a longer session (navigate, type, click, hover,
select, readers, `all`, screenshot, cookies, viewport, waits, back).

## 3. Local detector (`tools/detector/index.html`)

Served by `surf-testserver` at `/detector`, embedded in the binary for
`surf doctor --detector`. Each check writes `PASS` / `FAIL` / `INFO` into
`<li data-check=… data-status=… data-detail=…>`. `?mode=headed|headless`
decides which checks are informational. The page re-runs the globals and
prototype checks when `#recheck` is clicked, after the script has typed,
hovered and clicked — so any main-world injection by an action would be
caught.

| check | what it does | headed | headless |
|-------|--------------|--------|----------|
| `runtime-enable-getter` | `{get id(){…}}` and an Error with an own `stack` accessor passed to `console.debug` / `console.log`; a read = the inspector serialised the argument | PASS | PASS |
| `runtime-enable-stack` | a user `Error.prepareStackTrace` installed around `console.debug(new Error())`; invoked = the inspector formatted the error's stack | PASS | PASS |
| `webdriver` | `navigator.webdriver === false` | PASS | PASS |
| `headless-ua` | UA does not contain `HeadlessChrome` (headed); recorded headless | PASS | INFO — UA *does* say `HeadlessChrome/154.0.0.0` |
| `window-chrome` | `window.chrome` present (keys `loadTimes, csi, app`) | PASS | PASS |
| `globals` | `Object.keys(window)` / `getOwnPropertyNames(window)` diffed against a baseline taken in `<head>`, plus any name matching `surf`, `__`, `cdc_`, `puppeteer`, `playwright` | PASS (1235 names, none added) | PASS |
| `window-chrome-height` | `outerHeight − innerHeight` in 0..120 (headed: no automation infobar) | PASS — 87 px | INFO — 87 px (new headless emulates window chrome) |
| `native-prototypes` | `Function.prototype.toString.call` of `document.querySelector(All)`, `MutationObserver(.prototype.observe)`, `Function.prototype.toString`, `Object.getOwnPropertyDescriptor` is `[native code]` | PASS | PASS |
| `getter-timing` | 10 000 getter reads, `performance.now()` (informational) | INFO ≈ 1.0–1.4 ms | INFO ≈ 1.1 ms |

E2E: `tests/e2e/scripts/detector.surf` (headless) and
`detector-headed.surf` (headed; the runner passes `--headed` for
`*-headed.surf` and skips it without a display) emit every check and the
`.out` files expect every non-informational check `PASS`.

### What the Runtime.enable probe measures, honestly

Measured on Chrome 154 with a deliberately noisy client
(`tools/detector/control.py`: `--remote-debugging-port`,
`--enable-automation`, `Runtime.enable` on the page session, as Puppeteer /
Playwright / Selenium-CDP do):

```
PASS  runtime-enable-getter    getters never read
FAIL  runtime-enable-stack     Error.prepareStackTrace was invoked
FAIL  webdriver                navigator.webdriver = true
INFO  headless-ua              …HeadlessChrome/154.0.0.0…
PASS  window-chrome            keys: loadTimes,csi,app
PASS  globals                  1235 own names, none added, none suspicious
INFO  window-chrome-height     143px (outer 556, inner 413)
PASS  native-prototypes        all native
```

* The classic probe (an own `stack` accessor read by the console preview —
  what rebrowser's `runtimeEnableLeak` and the 2024 "brotector" snippet
  rely on) **no longer fires on Chrome 154**: V8 formats the error's stack
  internally for the preview instead of reading the property. The check is
  kept because older Chromes still leak there.
* What still leaks with `Runtime.enable` on current Chrome is that stack
  formatting itself: it calls a user `Error.prepareStackTrace`. That is the
  `runtime-enable-stack` check, and it discriminates (FAIL above, PASS
  under Surf).
* A `Symbol.toStringTag` getter or an Error's own `toString` also run during
  `console.debug` — but they run in *every* Chrome, inspected or not
  (verified with `--headless --dump-dom` and no CDP at all: both invoked).
  They are not a CDP signal and the detector does not use them.
* `webdriver` fails in the control because of `--enable-automation`; it also
  fails with the debugger port alone minus Surf's one switch (DECISIONS.md
  #12).

## 4. Public detectors, 2026-10-04, Chrome 154.0.8037.95, headed, 1280×1000

`tools/live-detectors.surf` opens each site in a Surf-launched stock Chrome,
screenshots it full-page into `tools/out/` (gitignored) and dumps the
visible text next to it. Run it with `surf run tools/live-detectors.surf`.

**bot-detector.rebrowser.net** — table of tests:

| test | result | note from the page |
|------|--------|--------------------|
| dummyFn | ⚪ not triggered | we called `typeof window.dummyFn` from the isolated world → `undefined`; the main-world function is not reachable |
| sourceUrlLeak | ⚪ not triggered | we called `document.getElementById('detections-json')` from the isolated world; the page's overridden getter is a main-world object and never ran |
| mainWorldExecution | ⚪ not triggered | we called `document.getElementsByClassName('div')` from the isolated world (→ 0); page text: "If you did and the test wasn't triggered, then you're running it in an isolated world, which is safe and not detectable." |
| runtimeEnableLeak | 🟢 | "No leak detected." |
| exposeFunctionLeak | ⚪ n/a | Surf has no `exposeFunction` |
| navigatorWebdriver | 🟢 | "No webdriver presented." |
| viewport | 🟢 | 1280×887, "different from default values used in automation libraries" |
| pwInitScripts | 🟢 | "No window.__pwInitScripts detected." |
| bypassCsp | 🟢 | CSP enabled |
| useragent | 🟢 | Chrome 154.0.8037.95 vs latest stable 154.0.8037.98 |

**bot.sannysoft.com** — every row:

| row | value |
|-----|-------|
| User Agent | `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) … Chrome/154.0.0.0 Safari/537.36` |
| WebDriver (New) | missing (passed) |
| WebDriver Advanced | passed |
| Chrome (New) | present (passed) |
| Permissions (New) | prompt |
| Plugins Length (Old) | 5 |
| Plugins is of type PluginArray | passed |
| Languages (Old) | en-US,en |
| WebGL Vendor / Renderer | Google Inc. (Apple) / ANGLE (Apple, ANGLE Metal Renderer: Apple M4 Max, Unspecified Version) |
| Broken Image Dimensions | 16x16 |
| Fingerprint Scanner (PHANTOM_*, HEADCHR_*, CHR_*, SELENIUM_DRIVER, SEQUENTUM, …) | 20 rows, all `ok`; none `failed`, none `warn` |

**browserscan.net/bot-detection** — verdict **Normal**. WebDriver, WebDriver
Advance, Selenium, NightmareJS, PhantomJS, Awesomium, Cef, CefSharp,
Coaches, FMiner, Born, Phantomas, Rhino, Webdriverio, Headless Chrome: all
*Normal*. "Chrome DevTools Protocol Detection": CDP *Normal*, Dev Tool
*Normal*. Navigator: `webdriver false`, `plugins [object PluginArray]`,
`hardwareConcurrency 14`, `platform MacIntel`, nothing flagged.

Nothing was red on any of the three pages. These are the detectors' own
public checks on one day; they change, so re-run the script before
quoting it.

## 5. Performance gates (`cargo test --release --test perf`)

| gate | budget | measured |
|------|--------|----------|
| `surf run tests/vm/noop.surf` process wall time (exec → exit), median of 20 | < 10 ms | **1.8 ms** (min 1.6, max 2.3); 3.1 ms when timed from Python |
| `surf` RSS while idle with one headless browser attached (`ps -o rss=`) | < 15 MB | **6.1 MB** |
| `surf run` start → first CDP frame sent (`--trace-cdp`, includes spawning Chrome) | reported, < 500 ms | **39–47 ms** (27 ms debug) |

Other timings from `surf doctor` (release): spawn → first
`Browser.getVersion` answered over the pipe **313 ms**; a `Browser.getVersion`
round trip **13–29 ms**; the shutdown ladder (`Browser.close` → exit →
profile removed) **96 ms**. The 50-concurrent-pages e2e test runs in
≈ 3 s end to end (debug).

Budgets are asserted only in release builds; a debug run prints the
numbers and says so (`tests/perf.rs`).

## 6. `surf doctor`

```
surf 0.1.0
platform: macos aarch64
chrome:   /Applications/Google Chrome.app/Contents/MacOS/Google Chrome (platform default)
version:  Google Chrome 154.0.8037.95
display:  yes (native window system)
launch:   ok — Chrome/154.0.8037.95 (pid …)
transport: pipe (fd 3 / fd 4) — spawn → first Browser.getVersion answered in 313 ms
cdp:      Browser.getVersion round trip 29 ms (protocol 1.3, …)
flags:    --remote-debugging-pipe --user-data-dir=… --no-first-run --no-default-browser-check --disable-blink-features=AutomationControlled --window-size=1280,800 --headless about:blank
profile:  … (temporary, removed on close)
close:    shutdown ladder finished in 96 ms
quiet:    no --enable-automation; only Page.enable is sent per page; never Runtime.enable / DOM.enable
```

`surf doctor --detector` additionally launches a browser the way a script
would (headed when a display exists), loads the embedded detector page
from a temp file, types / hovers / clicks, and prints the table; exit 1 if
any non-informational check fails. On Linux the display line also reports
`Xvfb` presence for `virtual: true`.

## 7. Known gaps

* **Headless UA.** New headless Chrome reports `HeadlessChrome/…` in
  `navigator.userAgent` (and `Browser.getVersion`). Surf does not rewrite
  it; `headless-ua` is informational in headless runs. Run headed (or
  `virtual: true` on Linux) when the UA matters.
* **`--disable-blink-features=AutomationControlled`** is on the command
  line of every launched browser. It is not visible from a page, but it is
  visible to anyone who can read the process's arguments. DECISIONS.md #12
  explains why it is the lesser side effect.
* **`Page.setLifecycleEventsEnabled`** is on for every page (needed to await
  `load` / `networkIdle`). It is a DevTools-only stream with no
  page-observable effect; listed for completeness.
* **Window chrome in headless** is emulated at 87 px (outer 800 / inner 713
  at `--window-size=1280,800`); a detector comparing it with a "real"
  headless (0 px) could notice. Informational.
* **The classic `Error.stack`-getter leak check does not discriminate on
  Chrome 154** (see §3). Detectors that still rely on it will show green
  for everyone on this Chrome; the `prepareStackTrace` variant is the one
  that matters now.
* **rebrowser's `exposeFunctionLeak`** cannot be exercised — Surf has no
  `exposeFunction`. Its main-world traps (`dummyFn`, `sourceUrlLeak`,
  `mainWorldExecution`) were called from the isolated world and stayed
  untriggered; a future opt-in main-world eval (TASKS.md) would have to be
  measured against them.
* Nothing here covers TLS / HTTP-level fingerprinting (JA3/JA4, header
  order), IP reputation, or behavioural scoring over time. Those are the
  browser's and the network's, not the protocol's; `engine: apostate`
  personas (TASKS.md) are the planned answer for the browser side.
