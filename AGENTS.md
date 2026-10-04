# Working agreement

For humans and agents working in this repo. Read `docs/architecture.md`
(crate map, contracts) and `docs/language.md` (spec) first; `DECISIONS.md`
lists what is settled.

## Crate map

| crate | owns | must not |
|-------|------|----------|
| `surf-syntax` | lexer, parser, AST, diagnostics | depend on tokio / `std::process` / `std::fs` (wasm32-clean) |
| `surf-vm` | bytecode compiler, async VM, `Value`, `Host` trait, stdlib | do IO; depend on tokio (wasm32-clean; `futures` only) |
| `surf-cdp` | transports, framing, connection/session mux, events, typed commands | know about pages, selectors, policy |
| `surf-browser` | discovery, launch flags, Xvfb, Browser/Page/World, auto-wait actions, input, network hooks, cookies | be `Send`; expose CDP method names above it |
| `surf-runtime` | `Host` impl, implicit pages, bare actions, handlers, tasks/actors/supervisors, lifetime | talk CDP directly |
| `surf-cli` | `surf run/check/doctor/repl`, exit codes | contain logic that belongs in runtime |

Dependency direction is strict: `cli → runtime → browser → cdp`, `runtime → vm → syntax`.

## Quiet-CDP rules (hard — judges grep for violations)

1. **Never** send `Runtime.enable`. Execution contexts come from
   `Page.createIsolatedWorld{frameId, worldName, grantUniveralAccess:true}`;
   evaluate with `Runtime.callFunctionOn{executionContextId}` /
   `Runtime.evaluate{contextId}`. World → runtime channel is
   `Runtime.addBinding{executionContextId}` + `Runtime.bindingCalled`.
2. **Never** pass `--enable-automation`. The **only** permitted
   `--disable-*` is `--disable-blink-features=AutomationControlled`, on
   browsers Surf launches (never when attaching) — Chrome sets
   `navigator.webdriver` merely because a debugger pipe/port is configured
   (DECISIONS.md #12). Any other `--disable-*` the user did not write is
   forbidden. The full flag list lives in
   `surf_browser::launch::LaunchConfig::args` and nowhere else.
   `--user-data-dir` is always passed (Chrome ≥ 136 ignores pipe/port on
   the default profile). `--headless` only when headless.
3. `Page.enable` (ref-counted) is the only domain on by default.
   `Network` / `Fetch` only while a hook / intercept / proxy-auth needs
   them, disabled when the last one goes. **Never** `DOM.enable` —
   `DOM.getContentQuads`, `DOM.scrollIntoViewIfNeeded`, `Input.*`,
   `Page.captureScreenshot`, `Storage.*`, `Emulation.*` work without it.
4. Pipe transport by default (fd 3 / fd 4, `\0`-terminated JSON). No
   listening port unless the script says `cdp: 9222`.
5. Scripts never inject into the main world. Helpers live in the isolated
   world under random names.

There is no `stealth:` flag and no "stealth" vocabulary. Quiet is the only
mode.

## Building and testing

```
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p surf-syntax -p surf-vm --target wasm32-unknown-unknown   # must stay green
```

Tests that need Chrome use `surf_browser::discovery::chrome_or_skip(name)`:
`SURF_CHROME=/path/to/chrome` overrides discovery (set-but-missing is an
error, not a fallback); otherwise `~/.cache/surf/chrome`, the platform
defaults (`/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`,
`/opt/google/chrome/chrome`, `google-chrome` on `PATH`, …) and the
Playwright / Puppeteer / Apostate caches are tried; when nothing is found
the test prints `skipping <name>: no Chrome/Chromium found …` with the
locations tried and passes. They **must** actually run on your machine —
do not merge on a skip. The e2e suite is `cargo test -p surf-cli --test
e2e`; the launcher's own suite is `cargo test -p surf-browser --test
launch` (needs a display for the headed test).

Apostate (a Chromium fork under `~/Library/Caches/apostate`) is **not** a
dependency. Never touch `~/apostate`. Surf must work with stock Chrome.

## Definition of done

fmt, clippy (`-D warnings`), tests green; Chrome-needing tests ran for real;
docs updated in the same commit as the behaviour (`docs/language.md` for
language changes, `docs/architecture.md` for contract changes, `DECISIONS.md`
for new decisions); deferred work added to `TASKS.md`; report what you
built, what you measured, what you deferred.

Three gates are part of "done", not extras:

| gate | command | what must hold |
|------|---------|----------------|
| e2e | `cargo test -p surf-cli --test e2e` | every `tests/e2e/scripts/<name>.surf` matches its `.out` / `.err` / `.code`; the port-exposure test (`lsof` while a browser is open over the pipe) is empty; 50 concurrent pages finish under 30 s. A new language feature or action ships with a script here. |
| perf | `cargo test --release -p surf-cli --test perf -- --test-threads=1` | `tests/perf.rs` budgets: noop cold start < 10 ms, idle RSS with one browser < 15 MB, start → first CDP frame < 500 ms. Update `docs/quiet-cdp.md` §5 and `docs/comparison.md` when the numbers move. |
| detector | `cargo test -p surf-cli --test e2e` (`detector`, `detector-headed` scripts) and `surf doctor --detector` | every non-informational check in `tools/detector/index.html` is `PASS`, headless and headed; `crates/surf-browser/tests/pages.rs::quiet_contract_only_page_enable_is_sent` still sees `Page.enable` as the only `*.enable`. Before quoting a public detector, re-run `tools/live-detectors.surf` and update `docs/quiet-cdp.md` §4 with the date and Chrome version. |

Anything that adds a CDP method, a launch flag or a domain enable is a
change to `docs/quiet-cdp.md` §1–2 in the same commit.

## Release procedure

Releases are built by `dist` (cargo-dist) from `.github/workflows/release.yml`,
which `dist generate` writes from `dist-workspace.toml`; do not hand-edit
the workflow. Targets: `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`; installers: shell + PowerShell; artifacts are
`surf-cli-<target>.tar.xz` / `.zip` containing the `surf` binary, plus
`surf-cli-installer.sh` / `.ps1`, `sha256.sum` and a source tarball.

1. `version = "x.y.z"` in `[workspace.package]` (every crate inherits it;
   the internal `[workspace.dependencies]` versions must match). Commit as
   `chore: release vx.y.z`.
2. The full definition of done above on that commit, on this Mac and in
   CI (`ci.yml`: fmt, clippy, wasm32, test + e2e on Linux and macOS, perf
   reported).
3. `dist plan` must list exactly one app (`surf-cli`; `surf-testserver` is
   `publish = false`), then `dist build --artifacts=local` for the host
   and run the archive's `surf doctor` once.
4. Re-measure what the docs quote: `docs/quiet-cdp.md` §4–5 (live
   detectors, perf gates) and `docs/comparison.md` (startup, RSS, CDP
   command counts) with the method sections there; update the date and
   Chrome version.
5. Tag `vx.y.z` on `main` and push the tag. The workflow builds every
   target, uploads the archives and installers, and creates the GitHub
   release (`contents: write`; no other secrets). A `-rc.N` suffix makes it
   a pre-release.
6. `cargo publish` in dependency order (`surf-syntax`, `surf-vm`,
   `surf-cdp`, `surf-browser`, `surf-runtime`, `surf-cli`) once the repo is
   public; `dist` does not publish to crates.io.

After changing `dist-workspace.toml` (targets, installers, the dist
version) run `dist generate` and commit the regenerated workflow. The
installer URLs come from `repository` in `Cargo.toml`.

## Roadmap

The seams for what comes next — `engine: apostate`, per-page personas via
process-per-persona + `Page::rebind`, the `pool:` / wasm build — are drawn
in `docs/architecture.md` ("Roadmap seams") and tracked at the top of
`TASKS.md`. Build against the seam; do not add a parallel mechanism.

## Commits

Small conventional commits: `feat(cdp): pipe transport`, `fix(vm): range
end`, `docs(language): clarify props`, `test(e2e): bindingCalled without
Runtime.enable`. No attribution trailers (no `Co-authored-by`, no
`Generated-by`). Never commit secrets, profiles, or `.empryo/` state.

## Style

- `#![forbid(unsafe_code)]` in every crate; if a transport truly needs
  `unsafe` (fd handling), isolate it in one module with `#![allow]` and a
  `// SAFETY:` comment.
- `#![warn(missing_docs)]` on library crates; every `pub` item has a doc
  line.
- Errors: `thiserror` enums in library crates; `surf_vm::RuntimeError`
  carries `span`, `selector`, `cdp_method`.
- Keep public contract signatures from `docs/architecture.md`; change them
  only together with the doc and every implementer.
