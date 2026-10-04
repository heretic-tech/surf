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
//! `surf-browser`. There is a debug assertion in [`Session::call_raw`] that
//! rejects `Runtime.enable` and `DOM.enable` so a mistake upstream fails
//! loudly in tests.
//!
//! This crate is `Send + Sync` (tokio::sync internally) so a future
//! thread-per-core runtime can shard connections (Decision 5).

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod connection;
pub mod error;
pub mod event;
pub mod protocol;
pub mod session;
pub mod transport;

pub use connection::Connection;
pub use error::CdpError;
pub use event::Event;
pub use protocol::Command;
pub use session::{DomainGuard, Session};
pub use transport::Transport;

/// Methods that must never be sent (they produce page-observable side
/// effects). Checked in debug builds by [`Session::call_raw`].
pub const FORBIDDEN_METHODS: &[&str] = &["Runtime.enable", "DOM.enable"];
