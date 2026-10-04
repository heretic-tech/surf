//! # surf-browser
//!
//! Drives a Chromium over `surf-cdp` without producing page-observable
//! side effects. This is where the **quiet rules** are enforced; nothing
//! above this crate talks CDP directly.
//!
//! Responsibilities:
//! - [`discovery`]: find a Chrome/Chromium binary (`browser.path`,
//!   `SURF_CHROME`, `~/.cache/surf/chrome`, platform defaults, then the
//!   Playwright / Puppeteer / Apostate caches) and its version.
//! - [`launch`]: build the argument list (see [`launch::LaunchConfig::args`]
//!   for the exact, exhaustive set of flags Surf passes), create the pipe
//!   pair, spawn the process, wire fd 3 / fd 4, capture a stderr tail,
//!   detect crashes, and run the shutdown ladder ([`launch::Launched::close`]).
//! - [`display`]: Linux `virtual: true` — manage an Xvfb server automatically.
//! - [`browser`]: the [`Browser`] handle: page registry (`page(n)` /
//!   `page("name")` auto-create, `sole_page` for bare actions), browser
//!   contexts for per-page proxies / isolation, graceful shutdown
//!   (`Browser.close` → wait → kill); one logical browser may later own
//!   several OS processes.
//! - [`page`]: the [`Page`] handle (`Rc<PageInner>`) whose backing
//!   `{session, target_id, browser_context_id, frame_id}` can be swapped
//!   by [`Page::rebind`] with cookie / storage / URL migration. Serves
//!   supervisor restarts, `shift_proxy()`, and future per-page personas.
//!   Navigation waits on `Page.lifecycleEvent`; dialogs follow a policy;
//!   screenshots, PDF, viewport, cookies live here too.
//! - [`world`]: isolated worlds via `Page.createIsolatedWorld` and
//!   evaluation through `Runtime.callFunctionOn{executionContextId}`;
//!   `Runtime.addBinding{executionContextId}` + `Runtime.bindingCalled`
//!   for the world → runtime channel. **Never `Runtime.enable`.**
//! - [`selector`]: CSS (default), `text=…`, `xpath=…` / `//…` resolution,
//!   via a helper installed in the isolated world under a random name.
//! - [`actions`]: auto-waiting actions as methods on [`Page`] / [`Element`]
//!   (attached → visible → stable → enabled, up to `timeout`): `click
//!   dblclick hover type fill press check uncheck select scroll focus text
//!   html attr value exists count all first wait wait_gone wait_text
//!   wait_url`.
//! - [`input`]: `Input.dispatchMouseEvent` / `dispatchKeyEvent` /
//!   `insertText` with coordinates from `DOM.getContentQuads` (no
//!   `DOM.enable`).
//! - [`network`]: `on request` / `on response` hooks, blocking, intercept,
//!   proxy auth. `Network` / `Fetch` are enabled only while a hook exists.
//! - [`cookies`]: `Storage.getCookies` / `Storage.setCookies`.
//! - [`observer`]: isolated-world `MutationObserver` for `on element_appears`.
//!
//! Only `Page.enable` is on by default (ref-counted, for lifecycle and
//! dialog events).

// `unsafe` is needed in exactly one place: `launch::sys` (`pre_exec` +
// `dup2` to put the pipe ends on fd 3 / fd 4, and `kill(SIGTERM)`). That
// module carries `#[allow(unsafe_code)]` and `// SAFETY:` comments.
#![deny(unsafe_code)]
#![warn(missing_docs)]

pub mod actions;
pub mod browser;
pub mod cookies;
pub mod discovery;
pub mod display;
pub mod error;
pub mod input;
pub mod launch;
pub mod network;
pub mod observer;
pub mod page;
pub mod selector;
pub mod util;
pub mod world;

pub use actions::{ActionOptions, Element};
pub use browser::{Browser, NewPageOptions, RebindTarget};
pub use cookies::Cookie;
pub use discovery::{chrome_or_skip, Found};
pub use display::VirtualDisplay;
pub use error::BrowserError;
pub use launch::{
    launch, CdpMode, LaunchConfig, LaunchOptions, Launched, ProxySpec, TransportChoice,
};
pub use page::{Backing, Dialog, DialogPolicy, Migration, Page, WaitUntil};
pub use selector::Selector;
pub use world::World;

/// Path-only discovery with no explicit override; `None` when nothing is
/// found. Convenience for callers that only need a path (`surf doctor`);
/// use [`discovery::find_chrome`] for the version, origin and the list of
/// locations tried.
pub fn find_chrome() -> Option<std::path::PathBuf> {
    discovery::find_chrome(None).ok().map(|f| f.path)
}
