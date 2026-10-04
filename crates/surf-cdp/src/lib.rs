//! # surf-cdp
//!
//! A thin, quiet Chrome DevTools Protocol client.
//!
//! Responsibilities:
//! - [`transport`]: byte transports. The default is the **pipe**
//!   (`--remote-debugging-pipe`: Chrome reads fd 3, writes fd 4; each message
//!   is UTF-8 JSON followed by a single `\0`). WebSocket is used only when the
//!   user writes `cdp: 9222` / `cdp: "ws://…"` or `pool: "wss://…"`.
//!   Windows uses `--remote-debugging-io-pipes=<read>,<write>`.
//! - [`connection`]: multiplexes request/response ids and `sessionId`s over
//!   one transport; owns the reader task.
//! - [`session`]: a flat or target-attached session: `call`, `call_raw`,
//!   typed `send`, per-method event streams, ref-counted domain enables.
//! - [`protocol`]: the [`protocol::Command`] trait plus typed structs
//!   generated from `protocol/*.json` for an allow-list of domains
//!   (Decision 11). Everything else goes through `call_raw`.
//!
//! ## Quiet rules enforced here
//! This crate never sends `Runtime.enable`, never enables `DOM`, and never
//! adds flags. It is deliberately dumb: it moves JSON frames. Policy lives in
//! `surf-browser`. [`Session::call_raw`] (and therefore `call` / `send`)
//! refuses [`FORBIDDEN_METHODS`] with a `CdpError::Protocol` in every
//! build, and the codegen does not emit typed commands for them, so a
//! mistake upstream fails loudly.
//!
//! This crate is `Send + Sync` (tokio::sync internally) so a future
//! thread-per-core runtime can shard connections (Decision 5).
//!
//! ## Tracing
//! `Connection::set_trace(true)` or `SURF_TRACE_CDP=1` logs every frame at
//! `debug` under the `surf_cdp::trace` target (`→` sent, `←` received).

// Unix needs no `unsafe` at all (tokio creates the pipes). Windows needs
// `CreatePipe` / `SetHandleInformation`, isolated in `transport::pipe::windows`
// with `SAFETY:` comments.
#![cfg_attr(not(windows), forbid(unsafe_code))]
#![cfg_attr(windows, deny(unsafe_code))]
#![warn(missing_docs)]

pub mod connection;
pub mod error;
pub mod event;
pub mod protocol;
pub mod session;
#[cfg(test)]
mod tests;
pub mod transport;

pub use connection::Connection;
pub use error::CdpError;
pub use event::Event;
pub use protocol::{Command, ProtocolEvent};
pub use session::{DomainGuard, Session};
pub use transport::Transport;

/// Methods that must never be sent (they produce page-observable side
/// effects). Refused by [`Session::call_raw`] in every build.
pub const FORBIDDEN_METHODS: &[&str] = &["Runtime.enable", "DOM.enable"];
