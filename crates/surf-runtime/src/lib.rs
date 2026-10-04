//! # surf-runtime
//!
//! The embedder: implements [`surf_vm::Host`] on top of `surf-browser` and
//! owns everything that makes the language's implicit rules work.
//!
//! Responsibilities:
//! - [`host`]: the [`Runtime`] type — `surf_vm::Host` impl, process-wide
//!   state (browsers, pages, tasks, handlers), `print` / `emit` sinks.
//! - [`config`]: `browser:` blocks → `surf_browser::LaunchOptions`
//!   (declarative; nothing launches until the first action).
//! - [`pages`]: implicit page resolution. `page` = the sole page of the
//!   default browser, auto-created on first use; `page(2)` / `page("login")`
//!   auto-create; bare actions resolve to the sole page; with several open,
//!   the error is `several pages are open (1, 2, "login") — say which:
//!   page(2).click(…)`. Each spawned task / actor / `parallel for` body
//!   that uses bare actions gets its own page.
//! - [`builtins`]: the bare-action globals (`goto click type … cookies
//!   set_cookie`), `page(…)`, `browser`, `env`, `sleep`, `shift_proxy`,
//!   `send` / `broadcast` / `receive` / `wait_for_message`.
//! - [`objects`]: `NativeObject` wrappers — `PageObject`, `ElementObject`,
//!   `BrowserObject`, `ActorRef`, `TaskHandle`, `RequestObject`, ….
//! - [`handlers`]: `on element_appears / navigation / dialog / request /
//!   response / message` registration and dispatch; each firing runs the
//!   body on its own task with a forked VM.
//! - [`tasks`]: `spawn`, `parallel for` (`limit`, `fail_fast`), `task`
//!   properties (`retry`, `on_fail`, `timeout`, `fresh`).
//! - [`actors`]: mailboxes, `self.id`, `send` / `broadcast` / `receive`.
//! - [`supervisors`]: `one_for_one` / `one_for_all`, `max_restarts`,
//!   `within`; restarts rebind the child's page.
//! - [`lifetime`]: a program stays alive while handlers are registered or
//!   tasks run; `exit` cancels every task and closes every browser.
//!
//! Single-threaded: tokio `current_thread` + `LocalSet`, `Rc` values.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod actors;
pub mod builtins;
pub mod config;
pub mod handlers;
pub mod host;
pub mod lifetime;
pub mod objects;
pub mod pages;
pub mod supervisors;
pub mod tasks;

pub use host::{Runtime, RuntimeOptions};

use std::rc::Rc;
use surf_vm::RuntimeError;

/// Parse, compile and run a script to completion (including waiting for
/// handlers / tasks per the lifetime rule). Returns the process exit code.
pub async fn run_source(name: &str, source: &str, opts: RuntimeOptions) -> Result<i32, RunError> {
    let program = surf_syntax::parse(name, source).map_err(RunError::Syntax)?;
    let compiled = surf_vm::compile_with_source(name, source, &program)
        .map_err(|e| RunError::Syntax(e.into_diagnostics(name)))?;
    let runtime = Runtime::new(opts);
    let globals = Rc::new(surf_vm::Globals::stdlib());
    let mut vm = surf_vm::Vm::new(runtime.clone(), globals);
    let result = vm.run(&compiled).await;
    let code = runtime.finish(result).await?;
    Ok(code)
}

/// Top-level failure of `run_source`.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// Lex / parse / compile error.
    #[error("{0}")]
    Syntax(surf_syntax::Diagnostics),
    /// Uncaught runtime error.
    #[error("{0}")]
    Runtime(#[from] RuntimeError),
}
