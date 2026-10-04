# Agent: bg-9 (code)
Task: # SURF — shared brief for every node (read fully before acting)

Repo: /Users/chaser/surf. Host: macOS arm64, cargo 1.97 stable, Google Chrome at `/Applications/Google Chrome.app/Contents/MacOS/Google Chrome`, Playwright Chromium builds under ~/Library/Caches/ms-playwright, Apostate (a Chromium fork

## Tool Calls
[shell]
[read] 
[read] 
[read] 
[read] 
[shell]
[shell]
[shell]
[read] 
[read] 
[shell]
[read] 
[shell]
[check_peers]
[edit_file] crates/surf-browser/src/error.rs
[edit_file] crates/surf-browser/src/error.rs
[edit_file] crates/surf-browser/src/util.rs
[edit_file] crates/surf-browser/src/world.rs
[shell]
[edit_file] crates/surf-browser/src/world.rs
[edit_file] crates/surf-browser/src/selector.rs
[edit_file] crates/surf-browser/src/selector.rs
[edit_file] crates/surf-browser/src/input.rs
[shell]
[edit_file] crates/surf-browser/src/cookies.rs
[read] 
[edit_file] crates/surf-browser/src/network.rs
[edit_file] crates/surf-browser/src/page.rs
[read] 
[edit_file] crates/surf-browser/src/page.rs
[edit_file] crates/surf-browser/src/page.rs
[edit_file] crates/surf-browser/src/actions.rs
[edit_file] crates/surf-browser/src/actions.rs
[edit_file] crates/surf-browser/src/browser.rs
[multi_edit] crates/surf-browser/src/browser.rs
[multi_edit] crates/surf-browser/src/page.rs
[multi_edit] crates/surf-browser/src/lib.rs
[shell]
[read] 
[multi_edit] crates/surf-browser/src/actions.rs
[grep] /TRACE_TARGET/
[shell]
[shell]
[edit_file] crates/surf-browser/tests/common/mod.rs
[edit_file] crates/surf-browser/tests/pages.rs
[multi_edit] crates/surf-browser/tests/pages.rs
[edit_file] crates/surf-browser/Cargo.toml
[shell]
[shell]
[shell]
[shell]
[shell]
[read] 
[multi_edit] crates/surf-browser/src/page.rs
[multi_edit] crates/surf-browser/src/actions.rs
[shell]
[read] 
[edit_file] crates/surf-browser/src/page.rs
[shell]
[edit_file] crates/surf-browser/tests/pages.rs
[shell]
[edit_file] crates/surf-browser/tests/pages.rs
[shell]
[multi_edit] crates/surf-browser/src/browser.rs
[edit_file] crates/surf-browser/src/page.rs
[shell]
[shell]
[shell]
[edit_file] crates/surf-browser/src/input.rs
[shell]
[shell]
[edit_file] TASKS.md
[edit_file] docs/architecture.md
[multi_edit] docs/architecture.md
[multi_edit] crates/surf-browser/src/lib.rs
[shell]
[shell]
[shell]

## Findings
**surf-browser task 4 API (pages/worlds/actions) landed:**
Commits ec96c83, e2ca4a7, 512a34f on main. `crates/surf-browser` now exposes (see docs/architecture.md "surf-browser: Browser, pages, worlds, actions"):
- `Browser::launch(LaunchOptions) -> Rc<Browser>` (CdpMode::Attach → `Browser::connect(ws_url, opts)`), `new_page(NewPageOptions{proxy, isolated, name})`, `page(n)` (auto-creates up to n), `page_named(name)`, `sole_page()` (0 → creates, >1 → `BrowserError::Ambiguous{names}` with Display "several pages are open (1, 2, \"login\") — say which: page(2).…"), `pages()`, `close_page`, `rebind_page(page, proxy, Migration)`, `close()`.
- `Page` methods (all `&self`, async): `goto(url, WaitUntil)`, `reload/back/forward(WaitUntil)`, `wait_for_navigation`, `url()`, `title()`, `eval(expr) -> serde_json::Value`, `eval_fn(src, args)`, `on_dialog(DialogPolicy)`, `dialogs()`, `screenshot(path, full)`, `screenshot_png`, `pdf(path)`, `set_viewport`, `cookies()/set_cookies(&[Cookie])/clear_cookies()`, `rebind`, `close`, `set_timeout`.
- Actions in `actions.rs` (methods on Page, `ActionOptions{timeout, delay}`): `click dblclick right_click hover focus scroll_into_view type_text fill press(key) press_on(sel,key) check uncheck select(sel,&[&str]) scroll_by scroll_to text html attr value exists count all first wait wait_gone wait_text wait_url wait_for`. `Element` (from `all`/`first`/`wait`) has the same methods minus the selector.
- Errors: `BrowserError::Timeout{action, selector, waited_ms, last_state}`, `Script{text, line}`, `Ambiguous{names}`, `PageClosed{index}`, `Navigation{url, reason}`.
- Old free functions `actions::goto/click/type_text/text` are gone (they were unimplemented stubs); surf-runtime's `PageObject` should call the Page methods.
- Quiet contract measured: only `Page.enable` is ever sent (tests/pages.rs).

## Agent Summary
`crates/surf-browser` task 4 is done — three commits on main (`ec96c83` feat, `e2ca4a7` tests, `512a34f` docs); `cargo fmt`, `cargo clippy --workspace --all-targets -D warnings`, and `cargo test --workspace` are green on this Mac with Chrome 154.

**Built**
- `browser.rs` — `Browser::launch` / `connect(ws_url)` (attach / `pool:`), page registry with `page(n)` (creates all missing up to n), `page_named`, `sole_page` (auto-creates page 1; `BrowserError::Ambiguous{names}` → `several pages are open (1, 2, 3, "login") — say which: page(2).…`), `new_page{proxy, isolated, name}` with `Target.createBrowserContext{proxyServer}`, `close_page`, `rebind_page`, idempotent `close` (attached browsers never closed).
- `page.rs` — `Page.enable` (DomainGuard) + `setLifecycleEventsEnabled` are the only enables on attach; `goto` waits for the lifecycle event matching the navigate `loaderId` (`load`/`DOMContentLoaded`/`networkIdle`/`commit`), `reload/back/forward/wait_for_navigation` via `frameNavigated` (+ bfcache restore); dialog policy task (`Accept` gives prompts their default, `Dismiss`, `AcceptWith`); screenshot/PDF (own base64 decoder, no new dep), viewport, `Storage.*` cookies; `rebind` migrates cookies + local/sessionStorage + URL and closes the old target.
- `world.rs` — `Page.createIsolatedWorld{random 12-char name, grantUniveralAccess}`, resolver installed as `__surf_<random>` on the world global, `callFunctionOn/evaluate` with `Script{text,line}` errors, `with_world` recreates once on `Cannot find context`; `add_binding`.
- `selector.rs` / `input.rs` / `actions.rs` — CSS/`text=`/xpath resolver (deepest element, exact-ci before substring, skips script/style); US key map + combos (`Shift+Tab`, `Ctrl+a`, `Mod+…`, macOS `commands`); every action polls 50 ms: attached → visible → stable over 2 rAF → enabled → `scrollIntoViewIfNeeded` → `getContentQuads` → `elementFromPoint` hit test → `Input.*`; timeouts report `last_state` (`not found`/`hidden`/`moving`/`disabled`/`covered by <…>`); `Element` handles released on drop.
- `network.rs` — `ProxyAuth`: `Fetch.enable{handleAuthRequests}` per page, answers `authRequired` (Proxy source) and `requestPaused`, drops the Fetch guard after the first proxy challenge.

**Measured**: 10 integration tests (fixture axum server, 8 HTML fixtures) cover every action/reader/wait, world recreation after a page-initiated navigation, `page(2)` auto-create, ambiguity, per-context cookie isolation, rebind migration, `Runtime.bindingCalled` without `Runtime.enable`, and a traced full session: 88 frames, the only `*.enable` is `Page.enable`.

**Deferred** (TASKS.md): proxy-auth test needs a 407 stub; task 9 must share one `Fetch.enable` between hooks and `ProxyAuth`; `fill` for date/number/file inputs; shadow DOM; `pdf()` headless-only message; frames API.

**Contract note**: the unimplemented free functions `actions::{goto,click,type_text,text}` were replaced by `Page` methods (documented in docs/architecture.md); surf-runtime only referenced the `Page` type, so nothing outside the crate changed.
