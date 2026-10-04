//! Byte transports and framing.

pub mod framing;
pub mod pipe;
pub mod ws;

use futures::future::BoxFuture;
use std::io;

/// A bidirectional stream of CDP frames (whole JSON messages, no
/// terminator). Implementations: [`pipe::PipeTransport`],
/// [`ws::WebSocketTransport`].
pub trait Transport: Send {
    /// Send one complete JSON message.
    fn send(&mut self, frame: &[u8]) -> BoxFuture<'_, io::Result<()>>;
    /// Receive the next complete JSON message; `Ok(None)` on clean EOF.
    fn recv(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>>;
}
