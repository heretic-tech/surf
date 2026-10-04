# Surf

[![ci](https://github.com/0xchasercat/surf/actions/workflows/ci.yml/badge.svg)](https://github.com/0xchasercat/surf/actions/workflows/ci.yml)
[![release](https://github.com/0xchasercat/surf/actions/workflows/release.yml/badge.svg)](https://github.com/0xchasercat/surf/actions/workflows/release.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

A tiny scripting language and runtime for driving Chrome over raw CDP with
zero boilerplate.

```
page.goto("https://example.com/login")
type("#username", "user")
type("#password", "pass")
click("#submit")
```

```
surf run login.surf
```

The same thing in Playwright:

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

No `await`, no browser/page objects to construct, no close. `page` is the
one tab you have; bare actions resolve to it and auto-wait. Errors name
the selector and the CDP method that failed. Startup is 3 ms (the VM) and
the driver process idles at 6 MB next to Chrome — numbers in
[docs/comparison.md](docs/comparison.md).

## Why

CDP is an IPC protocol — JSON frames over a pipe. Nothing inside a page's
V8 sandbox can observe the IPC layer. What "CDP detection" actually catches
are **side effects** of how automation frameworks use it:

- `--enable-automation` → `navigator.webdriver`, the infobar;
- `Runtime.enable` → console-serialisation getters fire and stack traces
  get captured (this is what Cloudflare / DataDome key on);
- main-world script injection;
- blanket `DOM.enable` and other heavy domains;
- headless rendering gaps.

Surf does not produce those side effects — not as a "stealth mode" but
because a transparent tool has no reason to. There is no `stealth:` flag.
Quiet is the only mode:

- pipe transport (`--remote-debugging-pipe`), no listening port unless asked;
- only `Page.enable` on by default; `Network`/`Fetch` only while a hook needs
  them; never `Runtime.enable`, never `DOM.enable`;
- all script evaluation in an isolated world created with
  `Page.createIsolatedWorld`; helpers under random names; never the main world;
- the exact launch flags: `--remote-debugging-pipe --user-data-dir=…
  --no-first-run --no-default-browser-check
  --disable-blink-features=AutomationControlled --window-size=W,H
  [--headless] [--proxy-server=…] <your flags> about:blank`. Nothing else
  (why the one `--disable-*`: [DECISIONS.md](DECISIONS.md) #12 — Chrome
  sets `navigator.webdriver` merely because a debugger pipe exists).

What that looks like on the wire, and what the public detectors say about
it, is in [docs/quiet-cdp.md](docs/quiet-cdp.md).

## Install

Release binaries (macOS arm64 / x86_64, Linux x86_64 / arm64, Windows
x86_64):

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/0xchasercat/surf/releases/latest/download/surf-cli-installer.sh | sh
```

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/0xchasercat/surf/releases/latest/download/surf-cli-installer.ps1 | iex"
```

Or from source with a Rust toolchain (≥ 1.85):

```
cargo install surf-cli            # or: cargo install --path crates/surf-cli
```

Then a Chrome. Stock Chrome is fine and is found automatically; `surf
install` fetches Chrome for Testing if you have none:

```
surf doctor                       # finds Chrome (or set SURF_CHROME=/path/to/chrome), times a launch
surf install                      # optional: Chrome for Testing → ~/.cache/surf/chrome/
surf run examples/hello.surf
```

## The language in thirty seconds

Indentation blocks, no braces, no `await`, no `let`. `page` is implicit;
bare actions (`goto click type text …`) resolve to the sole tab and
auto-wait. Strings interpolate with `{expr}`; durations are literals
(`500ms`, `2s`). `emit` writes a JSON line.

```
goto("https://example.com")
for a in all("a"):
    emit {text: a.text(), href: a.attr("href")}
```

Two tabs — `page(n)` auto-creates, names map to creation order
([examples/two-tabs.surf](examples/two-tabs.surf)):

```
page(1).goto("https://www.wikipedia.org")
page(2).goto("https://example.org")
page(1).type("input[name=search]", "surf")
print(page(2).title())
```

React to the page instead of polling it — an isolated-world
`MutationObserver` reports through `Runtime.bindingCalled`, no
`Runtime.enable`, no `DOM.enable`:

```
on element_appears(".cookie-banner"):
    event.first("button").click()

goto("https://example.com")
```

Concurrency without threads — each task gets its own tab, `task`s retry,
actors talk, a supervisor restarts what crashes
([examples/supervised.surf](examples/supervised.surf)):

```
task fetch(url):
    retry: 3
    on_fail: shift_proxy()
    goto(url)
    return title()

parallel for url in urls:
    limit: 4
    emit {url: url, title: fetch(url)}

actor Scout():
    goto("https://example.com")
    broadcast({token: eval("document.title")})

actor Worker(n):
    msg = receive(timeout: 30s)
    goto("https://example.com/?t={msg.token}")
    emit {worker: n, title: title()}

supervisor Crew:
    strategy: one_for_one
    max_restarts: 3
    spawn Scout()
    parallel for n in 1..=3:
        spawn Worker(n)
```

Full reference with a cheatsheet: [docs/language.md](docs/language.md).
Design: [docs/architecture.md](docs/architecture.md),
[DECISIONS.md](DECISIONS.md). Measured against Playwright, Puppeteer,
chromedp and chromiumoxide: [docs/comparison.md](docs/comparison.md).
More scripts in [examples/](examples/) and
[tests/e2e/scripts/](tests/e2e/scripts/).

## Status

v0.1.0. One browser, one tab is the 90 % case and needs no setup; multiple
tabs, browsers, proxies, hooks, interception, downloads, actors and
supervisors are in. Known gaps and the roadmap (`engine: apostate`,
per-page personas, a wasm build) are in [TASKS.md](TASKS.md).

## License

MIT — see [LICENSE](LICENSE).
