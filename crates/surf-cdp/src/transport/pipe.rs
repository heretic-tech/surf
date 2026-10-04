//! Pipe transport (`--remote-debugging-pipe`).
//!
//! Chrome reads commands from fd 3 and writes events/responses to fd 4
//! (on Windows: inheritable handles passed via
//! `--remote-debugging-io-pipes=<readHandle>,<writeHandle>`). `surf-browser`
//! creates the pipe pair with [`create_pair`], hands the child ends to the
//! spawned process, and wraps the parent ends in a [`PipeTransport`].
//! Implemented in task 2.

use super::framing::FrameDecoder;
use super::Transport;
use futures::future::BoxFuture;
use std::io;
use tokio::io::{AsyncRead, AsyncWrite};

/// The child's ends of the pipe pair, to be installed as fd 3 / fd 4 (or
/// passed as handles) when spawning Chrome.
#[derive(Debug)]
pub struct ChildEnds {
    /// Raw fd / handle Chrome will read from (its fd 3).
    pub child_read: u64,
    /// Raw fd / handle Chrome will write to (its fd 4).
    pub child_write: u64,
}

/// Parent-side transport over the pipe pair.
pub struct PipeTransport {
    reader: Box<dyn AsyncRead + Send + Unpin>,
    writer: Box<dyn AsyncWrite + Send + Unpin>,
    decoder: FrameDecoder,
}

impl PipeTransport {
    /// Wrap arbitrary async streams (useful for tests with in-memory duplexes).
    pub fn from_streams(
        reader: Box<dyn AsyncRead + Send + Unpin>,
        writer: Box<dyn AsyncWrite + Send + Unpin>,
    ) -> Self {
        Self {
            reader,
            writer,
            decoder: FrameDecoder::new(),
        }
    }
}

/// Create a pipe pair. Returns the parent transport and the child ends.
pub fn create_pair() -> io::Result<(PipeTransport, ChildEnds)> {
    Err(io::Error::other(
        "pipe transport not implemented yet (task 2)",
    ))
}

impl Transport for PipeTransport {
    fn send(&mut self, frame: &[u8]) -> BoxFuture<'_, io::Result<()>> {
        let _ = (frame, &mut self.writer);
        Box::pin(async {
            Err(io::Error::other(
                "pipe transport not implemented yet (task 2)",
            ))
        })
    }

    fn recv(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>> {
        let _ = (&mut self.reader, &mut self.decoder);
        Box::pin(async {
            Err(io::Error::other(
                "pipe transport not implemented yet (task 2)",
            ))
        })
    }
}
