# Surf

A tiny scripting language and runtime for driving Chrome over raw CDP with
zero boilerplate.

```
browser:
    virtual: true

page.goto("https://example.com/login")
type("#username", "user")
type("#password", "pass")
click("#submit")
```

```
surf run login.surf
```

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
  --no-first-run --no-default-browser-check --window-size=W,H [--headless]
  [--proxy-server=…] <your flags> about:blank`. Nothing else.

## The language in thirty seconds

Indentation blocks, no braces, no `await`, no `let`. `page` is implicit;
bare actions (`goto click type text …`) resolve to the sole tab and
auto-wait. Strings interpolate with `{expr}`; durations are literals
(`500ms`, `2s`). `emit` writes a JSON line. `on element_appears(".x"):`
reacts; `spawn`, `parallel for`, `task`, `actor`, `supervisor` give you
concurrency without threads.

```
goto("https://example.com")
for a in all("a"):
    emit {text: a.text(), href: a.attr("href")}
```

Full reference: [docs/language.md](docs/language.md). Design:
[docs/architecture.md](docs/architecture.md), [DECISIONS.md](DECISIONS.md).
More scripts in [examples/](examples/).

## Install

Requires a Rust toolchain and a Chrome / Chromium (stock Chrome is fine).

```
cargo install --path crates/surf-cli
surf doctor          # finds Chrome (or set SURF_CHROME=/path/to/chrome)
surf run examples/hello.surf
```

## Status

v0.1 scaffold. Language spec and crate contracts are fixed; the lexer,
parser, VM, CDP client and browser driver are being implemented against
them. See [TASKS.md](TASKS.md).

## License

MIT — see [LICENSE](LICENSE).
