# Surf next to Playwright, Puppeteer, chromedp, chromiumoxide

Same job, five tools. Everything in the "measured" rows was measured on
one machine on one day; the method is at the bottom so you can re-run it.
Nothing here is a reason to pick a tool for a test suite — Playwright and
Puppeteer are far more complete. The rows are about what each tool makes
Chrome *do* and what that costs.

Machine: macOS 15 arm64 (Apple M4 Max), Google Chrome 154.0.8037.95
stable (the same binary for every tool, `executablePath` / `SURF_CHROME`),
headless, 2026-10-04. Surf 0.1.0 release build; Node 26.8.1 with
Playwright 1.63.0 and puppeteer-core 25.12.0. chromedp and chromiumoxide
were read, not run (their launch flags and init commands are static lists
in `allocate.go` / `chromedp.go` and `browser/config.rs` /
`handler/{frame,network,target}.rs`); where a row needs a number they say
*not measured*.

## The login example

Surf — `examples/login.surf`, 4 lines of script (+ the optional `browser:`
block):

```
page.goto("https://example.com/login")
type("#username", "user")
type("#password", "pass")
click("#submit")
```

Playwright (Node):

```js
const { chromium } = require('playwright');

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage();
  await page.goto('https://example.com/login');
  await page.fill('#username', 'user');
  await page.fill('#password', 'pass');
  await page.click('#submit');
  await browser.close();
})();
```

Puppeteer (Node):

```js
const puppeteer = require('puppeteer');

(async () => {
  const browser = await puppeteer.launch();
  const page = await browser.newPage();
  await page.goto('https://example.com/login');
  await page.type('#username', 'user');
  await page.type('#password', 'pass');
  await page.click('#submit');
  await browser.close();
})();
```

chromedp (Go):

```go
package main

import (
	"context"

	"github.com/chromedp/chromedp"
)

func main() {
	ctx, cancel := chromedp.NewContext(context.Background())
	defer cancel()
	if err := chromedp.Run(ctx,
		chromedp.Navigate("https://example.com/login"),
		chromedp.SendKeys("#username", "user"),
		chromedp.SendKeys("#password", "pass"),
		chromedp.Click("#submit"),
	); err != nil {
		panic(err)
	}
}
```

chromiumoxide (Rust):

```rust
use chromiumoxide::browser::{Browser, BrowserConfig};
use futures::StreamExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (mut browser, mut handler) =
        Browser::launch(BrowserConfig::builder().build()?).await?;
    let h = tokio::spawn(async move { while handler.next().await.is_some() {} });
    let page = browser.new_page("https://example.com/login").await?;
    page.find_element("#username").await?.click().await?.type_str("user").await?;
    page.find_element("#password").await?.click().await?.type_str("pass").await?;
    page.find_element("#submit").await?.click().await?;
    browser.close().await?;
    h.await?;
    Ok(())
}
```

| | Surf | Playwright | Puppeteer | chromedp | chromiumoxide |
|---|---|---|---|---|---|
| lines for the login example | **4** (7 with `browser:`) | 11 | 11 | 19 | 16 (+ `Cargo.toml`) |
| explicit browser / page objects | no — `page` is implicit, bare actions resolve to the sole tab | `browser`, `context`, `page` | `browser`, `page` | `ctx` | `browser`, `handler`, `page` |
| `await` / error plumbing | none (blocking is invisible; errors carry selector + CDP method) | `await` on every call | `await` | `if err != nil` | `?` on every call, plus the handler task |

## Measured

| | Surf | Playwright | Puppeteer | chromedp | chromiumoxide |
|---|---|---|---|---|---|
| startup, no browser: process start → exit, median of 20 | **3.0 ms** (`surf run noop.surf`; 2.6–3.8) | 135 ms (`node -e "require('playwright')"`; 132–143) | 73 ms (`node -e "require('puppeteer-core')"`; 70–95) | not measured (compiled Go binary; expect single-digit ms) | not measured (compiled Rust binary; expect single-digit ms) |
| launch Chrome → `goto("about:blank")` → `title()` → close, median of 20 | **488 ms** (462–582) | 548 ms (509–614) | 453 ms (432–503) | not measured | not measured |
| driver RSS while idle with one headless browser open (`ps -o rss=`) | **6.1 MB** | 136 MB (`node` + playwright) | 84 MB (`node` + puppeteer-core) | not measured | not measured |
| start → first CDP frame on the wire | 24 ms (`--trace-cdp`; 39–47 ms in `tests/perf.rs`) | — | — | — | — |
| CDP commands sent for that script | **11** | 24 | 27 | — | — |

The middle row is dominated by Chrome itself (process spawn ≈ 300 ms to
the first `Browser.getVersion` answer, then its shutdown). Surf's own
overhead is the first and last rows; the ≈ 35 ms it loses to Puppeteer
end-to-end is its shutdown ladder waiting for the temp profile to be
removable (`docs/quiet-cdp.md` §5: 96 ms), which Puppeteer does not wait
for.

## What each tool makes Chrome do

| | Surf | Playwright 1.63 | Puppeteer 25.12 | chromedp (master) | chromiumoxide (main) |
|---|---|---|---|---|---|
| CDP domains enabled on a fresh page | **`Page` only** (+ `Page.setLifecycleEventsEnabled`); `Network` / `Fetch` only while a hook needs them | `Page`, `Runtime`, `Network`, `Log` (+ `Page.addScriptToEvaluateOnNewDocument`, `setLifecycleEventsEnabled`, `Emulation.*`, `Browser.setDownloadBehavior`) | `Page`, `Runtime`, `Network`, `Log`, `Audits`, `Performance`, `WebMCP` (+ `addScriptToEvaluateOnNewDocument`, `setLifecycleEventsEnabled`, `Emulation.*`) | `Runtime`, `Log`, `Network`, `Inspector`, `Page`, `DOM`, `CSS` (+ `setLifecycleEventsEnabled`) | `Page`, `Runtime`, `Network`, `Performance`, `Log` (+ `setLifecycleEventsEnabled`) |
| `Runtime.enable` | **never** (refused by `surf_cdp::FORBIDDEN_METHODS`; contexts come from `Page.createIsolatedWorld`) | yes | yes | yes | yes |
| `DOM.enable` | **never** | no | no | yes | no |
| `--enable-automation` | **no** | no (dropped; but see next row) | yes | yes | yes |
| `navigator.webdriver` on a launched browser | `false` (`--disable-blink-features=AutomationControlled`, DECISIONS.md #12) | `true` (the debugger pipe alone turns it on) | `true` | `true` | `true` |
| launch flags, headless default | **8** (`--remote-debugging-pipe --user-data-dir --no-first-run --no-default-browser-check --disable-blink-features=AutomationControlled --window-size --headless about:blank`) | 45 (26 `--disable-*`, incl. `--disable-features=…` with 15 features, `--no-sandbox`, `--hide-scrollbars`, `--mute-audio`) | 35 (23 `--disable-*`, `--hide-scrollbars`, `--mute-audio`, `--enable-automation`) | ≈ 26 (`DefaultExecAllocatorOptions`: 18 `--disable-*`, `--enable-automation`, `--headless`, …) | 24 (`DEFAULT_ARGS`: 16 `--disable-*`, `--enable-automation`, …) |
| main-world script injection by default | **none**; helpers live in a random-named isolated world | `Page.addScriptToEvaluateOnNewDocument` (utility bindings) | `Page.addScriptToEvaluateOnNewDocument` | no | `Page.addScriptToEvaluateOnNewDocument` on each new page |
| transport | **pipe** (fd 3 / fd 4, `\0`-framed JSON); websocket only for `cdp: 9222` / `cdp: "ws://…"` / `pool:` | pipe | websocket (`--remote-debugging-port=0`); pipe opt-in | websocket (`--remote-debugging-port=0`); pipe opt-in | websocket (`--remote-debugging-port`) |
| listening TCP port by default | **none** | none | yes (localhost) | yes (localhost) | yes (localhost) |
| isolated world for the user's own evaluation | always (`eval`, selectors, observers) | utility world for internals; `page.evaluate` runs in the **main** world | main world | main world | main world |

Enable lists and flags for Playwright and Puppeteer come from the traces
in the method section below; chromedp's from `chromedp.go` (`runtime.Enable`
then `log, network, inspector, page, dom, css`) and `allocate.go`;
chromiumoxide's from `handler/frame.rs::init_commands`,
`handler/network.rs::init_commands`, `handler/target.rs::page_init_commands`
and `browser/config.rs::DEFAULT_ARGS`. Playwright 1.63 no longer passes
`--enable-automation`; `navigator.webdriver` is still `true` under it
because a configured debugger pipe/port turns Blink's `AutomationControlled`
feature on by itself (measured in DECISIONS.md #12).

## Model

| | Surf | Playwright | Puppeteer | chromedp | chromiumoxide |
|---|---|---|---|---|---|
| language | its own (indentation blocks, no `await`) | JS/TS, Python, Java, .NET | JS/TS | Go | Rust |
| concurrency | single thread, `spawn`, `parallel for … limit:`, each task gets a private page automatically | promises; one page per `Promise.all` arm, by hand | promises | goroutines + contexts | tokio tasks |
| fault tolerance | `task` with `retry: / on_fail: / timeout: / fresh:`, actors with mailboxes, `supervisor` with `one_for_one` / `one_for_all` restarts, page identity survives a restart (`Page::rebind`) | test-runner retries; none in the library | none | none | none |
| proxy rotation | `proxies: […]` + `shift_proxy()` moves the page to a new context behind the next proxy, cookies kept | new context per proxy, by hand | new browser per proxy | new allocator | new browser |
| reactive DOM | `on element_appears(sel):` (isolated-world `MutationObserver` → `Runtime.bindingCalled`, no `DOM.enable`) | `locator.waitFor` / `page.waitForSelector` (polling in the utility world) | `waitForSelector` (polling) | `WaitVisible` (polling) | polling |
| network hooks | `on request` / `on response` / `intercept(pattern):` / `block([…])`; domains enabled only while used | `page.route`, `page.on('request')` (`Network` always on) | `setRequestInterception`, `page.on('request')` | `ListenTarget` | event streams |
| remote browsers | `pool: "wss://…"` / `cdp: "ws://…"` — same script | `connectOverCDP` / `connect` | `connect` | `RemoteAllocator` | `Browser::connect` |
| cold start | bytecode VM, ≈ 2 ms to the first statement | Node + ≈ 130 ms of module loading | Node + ≈ 70 ms | compiled | compiled |
| binary / install | one 6.9 MB static binary; `surf install` fetches Chrome for Testing | `npm i playwright` + `npx playwright install` (its own browser builds) | `npm i puppeteer` (downloads Chrome for Testing) | Go module | crate |

## Method

Everything below was run from one directory with the Node packages
installed locally (`npm i playwright@1.63.0 puppeteer-core@25.12.0`).

* **Startup, no browser.** `surf run noop.surf` where `noop.surf` is
  `x = 1`; `node -e "require('playwright')"`; `node -e
  "require('puppeteer-core')"`. Wall time of the child from `Popen` to
  exit, 20 runs, median / min / max.
* **Launch → title → close.** `surf run title.surf` with

  ```
  browser:
      headless: true

  goto("about:blank")
  print(title())
  ```

  against `node -e` scripts that `launch({executablePath, headless: true})`,
  `newPage()`, `goto('about:blank')`, `title()`, `close()`. Same Chrome
  binary for all three (`SURF_CHROME` / `executablePath`), 20 runs each,
  interleaved by tool, nothing else building on the machine.
* **RSS.** The same scripts with a 4 s sleep after `goto`; `ps -o rss=`
  on the driver process (not Chrome) 2.5 s in.
* **CDP traces.** Surf: `surf run --trace-cdp title.surf`, counting `→`
  frames by method (11: `Browser.getVersion`, `Target.createTarget`,
  `Target.attachToTarget`, `Page.enable`, `Page.setLifecycleEventsEnabled`,
  `Page.getFrameTree`, `Page.navigate`, `Page.createIsolatedWorld`,
  `Runtime.evaluate`, `Runtime.callFunctionOn`, `Browser.close`).
  Playwright: `DEBUG=pw:protocol,pw:browser`, counting `SEND ►` frames,
  argv from the `<launching>` line. Puppeteer: `CdpCDPSession.prototype.send`
  and `Connection.prototype.send` wrapped to count methods, argv from
  `browser.process().spawnargs`.
* **Detector results** for Surf (rebrowser, sannysoft, browserscan, and
  the local replica) are in `docs/quiet-cdp.md` §3–4 with the same date
  and Chrome.

The numbers will differ on your machine; the ratios should not. Re-run
before quoting.
