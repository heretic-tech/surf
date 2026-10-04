//! Byte transports and framing.
//!
//! A [`Transport`] moves whole CDP messages (UTF-8 JSON, no terminator) in
//! both directions. The connection layer drives one transport from a single
//! task with `tokio::select!`, so `recv` **must be cancel-safe**: a `recv`
//! future that is dropped before completion must not lose bytes. Both
//! built-in transports satisfy this (`AsyncReadExt::read` and
//! `StreamExt::next` are cancel-safe and the pipe decoder buffers partial
//! frames synchronously after each read).
//!
//! | transport | when | framing |
//! |-----------|------|---------|
//! | [`pipe::PipeTransport`] | default (`--remote-debugging-pipe`) | JSON + `\0` |
//! | [`ws::WsTransport`] | `cdp: 9222`, `cdp: "ws://…"`, `pool: "wss://…"` | one text frame per message |
//!
//! [`tcp::connect`] turns `host:port` into a [`ws::WsTransport`] by reading
//! `/json/version`.

pub mod fake;
pub mod framing;
pub mod pipe;
pub mod tcp;
pub mod ws;

use futures::future::BoxFuture;
use std::io;

/// A bidirectional stream of CDP frames (whole JSON messages, no
/// terminator). Implementations: [`pipe::PipeTransport`],
/// [`ws::WsTransport`].
///
/// `recv` must be cancel-safe (see the module docs).
pub trait Transport: Send {
    /// Send one complete JSON message.
    fn send(&mut self, frame: &[u8]) -> BoxFuture<'_, io::Result<()>>;
    /// Receive the next complete JSON message; `Ok(None)` on clean EOF.
    fn recv(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>>;
}
