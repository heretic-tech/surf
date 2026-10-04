//! # surf-browser
//!
//! Drives a Chromium over `surf-cdp` without producing page-observable
//! side effects. This is where the **quiet rules** are enforced; nothing
//! above this crate talks CDP directly.
//!
//! Responsibilities:
//! - [`discovery`]: find a Chrome/Chromium binary (`SURF_CHROME` env,
//!   `browser.path`, then platform defaults).
//! - [`launch`]: build the argument list (see [`launch::LaunchOptions::args`]
//!   for the exact, exhaustive set of flags Surf passes), create the pipe
//!   pair, spawn the process, wire fd 3 / fd 4, capture a stderr tail.
//! - [`xvfb`]: Linux `virtual: true` — manage an Xvfb server automatically.
//! - [`browser`]: the [`Browser`] handle: browser contexts, target
//!   lifecycle, proxy rotation, graceful shutdown (`Browser.close` → wait →
//!   kill), one logical browser may own several OS processes.
//! - [`page`]: the [`Page`] handle (`Rc<PageInner>`) whose backing
//!   `{session, target_id, browser_context_id, frame_id}` can be swapped
//!   by [`Page::rebind`] with cookie / storage / URL migration. Serves
//!   supervisor restarts, `shift_proxy()`, and future per-page personas.
//! - [`world`]: isolated worlds via `Page.createIsolatedWorld` and
//!   evaluation through `Runtime.callFunctionOn{executionContextId}`;
//!   `Runtime.addBinding{executionContextId}` + `Runtime.bindingCalled`
//!   for the world → runtime channel. **Never `Runtime.enable`.**
//! - [`selector`]: CSS (default), `text=…`, `xpath=…` / `//…` resolution,
//!   via a helper installed in the isolated world under a random name.
//! - [`actions`]: auto-waiting actions (attached → visible → stable →
//!   enabled, up to `timeout`): `goto click type fill press hover check
//!   select scroll text html attr value exists count all wait wait_gone
//!   wait_text wait_url eval screenshot pdf url title back reload …`.
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

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod actions;
pub mod browser;
pub mod cookies;
pub mod discovery;
pub mod error;
pub mod input;
pub mod launch;
pub mod network;
pub mod observer;
pub mod page;
pub mod selector;
pub mod world;
pub mod xvfb;

pub use browser::Browser;
pub use discovery::find_chrome;
pub use error::BrowserError;
pub use launch::{CdpMode, LaunchOptions};
pub use page::{Backing, Migration, Page};
pub use selector::Selector;
pub use world::World;
