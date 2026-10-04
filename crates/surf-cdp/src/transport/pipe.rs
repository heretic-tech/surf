//! Pipe transport (`--remote-debugging-pipe`).
//!
//! Chrome reads commands from **fd 3** and writes events/responses to
//! **fd 4**. Two anonymous pipes are involved:
//!
//! ```text
//!   pipe A:  parent write end  ──►  child read end   (child's fd 3)
//!   pipe B:  child write end   ──►  parent read end  (child's fd 4)
//! ```
//!
//! [`create_pair`] builds both and returns the parent's halves wrapped in a
//! [`PipeTransport`] plus the child's halves as [`ChildFds`]. The launcher
//! (`surf-browser`) is responsible for installing the child halves:
//!
//! * **unix** — every fd we create is `CLOEXEC` (tokio opens them that way).
//!   Keep them that way: in `Command::pre_exec` call
//!   `dup2(child.read, 3)` and `dup2(child.write, 4)`. `dup2` clears
//!   `CLOEXEC` on the *new* descriptor only, so 3 and 4 survive `exec` and
//!   the original (higher-numbered) copies are closed automatically. The
//!   child ends are converted to **blocking** mode, which is what Chrome's
//!   pipe reader/writer threads expect. After `spawn` returns, drop
//!   [`ChildFds`] so the parent does not keep the child's ends open —
//!   otherwise the parent would never see EOF when Chrome exits.
//! * **windows** — the two child handles are created inheritable
//!   (`SetHandleInformation(HANDLE_FLAG_INHERIT)`); pass
//!   [`ChildFds::io_pipes_arg`] (`--remote-debugging-io-pipes=<read>,<write>`,
//!   handles serialised as unsigned integers) on the command line and drop
//!   [`ChildFds`] after `spawn`.
//!
//! Framing on the wire is ASCIIZ: UTF-8 JSON followed by one `0x00`
//! (see [`super::framing`]). The reader copes with a message split across
//! reads and several messages in one read.

use super::framing::{encode_frame, FrameDecoder};
use super::Transport;
use futures::future::BoxFuture;
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Read buffer size for one `read` call on the pipe.
const READ_CHUNK: usize = 64 * 1024;

/// Parent-side transport over the pipe pair.
pub struct PipeTransport {
    reader: Box<dyn AsyncRead + Send + Unpin>,
    writer: Box<dyn AsyncWrite + Send + Unpin>,
    decoder: FrameDecoder,
    read_buf: Vec<u8>,
    write_buf: Vec<u8>,
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
            read_buf: vec![0; READ_CHUNK],
            write_buf: Vec::new(),
        }
    }
}

impl std::fmt::Debug for PipeTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeTransport")
            .field("pending_bytes", &self.decoder.pending())
            .finish()
    }
}

impl Transport for PipeTransport {
    fn send(&mut self, frame: &[u8]) -> BoxFuture<'_, io::Result<()>> {
        self.write_buf.clear();
        encode_frame(frame, &mut self.write_buf);
        Box::pin(async move {
            self.writer.write_all(&self.write_buf).await?;
            self.writer.flush().await
        })
    }

    /// Cancel-safe: the only await is `read`, and bytes it returns are fed
    /// to the decoder synchronously before the next await point.
    fn recv(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>> {
        Box::pin(async move {
            loop {
                if let Some(frame) = self.decoder.next_frame() {
                    return Ok(Some(frame));
                }
                let n = self.reader.read(&mut self.read_buf).await?;
                if n == 0 {
                    return Ok(None);
                }
                self.decoder.feed(&self.read_buf[..n]);
            }
        })
    }
}

#[cfg(unix)]
pub use unix::{create_pair, ChildFds};
#[cfg(windows)]
pub use windows::{create_pair, ChildFds};

#[cfg(unix)]
mod unix {
    use super::PipeTransport;
    use std::io;
    use std::os::fd::{AsRawFd, OwnedFd, RawFd};
    use tokio::net::unix::pipe;

    /// The child's ends of the pipe pair. Both are `CLOEXEC` and blocking;
    /// `dup2` them onto 3 and 4 in `pre_exec` (see the module docs) and drop
    /// this value after `spawn`.
    #[derive(Debug)]
    pub struct ChildFds {
        /// Chrome reads commands from this (must become its fd 3).
        pub read: OwnedFd,
        /// Chrome writes responses/events to this (must become its fd 4).
        pub write: OwnedFd,
    }

    impl ChildFds {
        /// Raw descriptor numbers `(read, write)` for `dup2` in `pre_exec`.
        pub fn raw(&self) -> (RawFd, RawFd) {
            (self.read.as_raw_fd(), self.write.as_raw_fd())
        }
    }

    /// Create the two pipes. Returns the parent transport and the child ends.
    pub fn create_pair() -> io::Result<(PipeTransport, ChildFds)> {
        // pipe A: parent → child (child's fd 3)
        let (to_child, child_read) = pipe::pipe()?;
        // pipe B: child → parent (child's fd 4)
        let (child_write, from_child) = pipe::pipe()?;
        let child = ChildFds {
            read: child_read.into_blocking_fd()?,
            write: child_write.into_blocking_fd()?,
        };
        let transport = PipeTransport::from_streams(Box::new(from_child), Box::new(to_child));
        Ok((transport, child))
    }
}

#[cfg(windows)]
mod windows {
    //! Windows anonymous pipes. `CreatePipe` handles do not support
    //! overlapped IO, so the parent ends are wrapped in `tokio::fs::File`
    //! (blocking reads/writes on tokio's blocking pool). Compiled but not
    //! yet exercised by CI — see TASKS.md.
    #![allow(unsafe_code)]

    use super::PipeTransport;
    use std::io;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::Foundation::{
        SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::System::Pipes::CreatePipe;

    /// The child's ends of the pipe pair: inheritable handles. Pass
    /// [`ChildFds::io_pipes_arg`] on the command line and drop this value
    /// after `spawn`.
    #[derive(Debug)]
    pub struct ChildFds {
        /// Chrome reads commands from this handle.
        pub read: OwnedHandle,
        /// Chrome writes responses/events to this handle.
        pub write: OwnedHandle,
    }

    impl ChildFds {
        /// Raw handle values `(read, write)` as unsigned integers.
        pub fn raw(&self) -> (usize, usize) {
            (
                self.read.as_raw_handle() as usize,
                self.write.as_raw_handle() as usize,
            )
        }

        /// The `--remote-debugging-io-pipes=<read>,<write>` argument.
        pub fn io_pipes_arg(&self) -> String {
            let (r, w) = self.raw();
            format!("--remote-debugging-io-pipes={r},{w}")
        }
    }

    /// `(read, write)` handles of one anonymous pipe, both non-inheritable.
    fn create_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
        let mut read: HANDLE = INVALID_HANDLE_VALUE;
        let mut write: HANDLE = INVALID_HANDLE_VALUE;
        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: 0,
        };
        // SAFETY: out-pointers are valid for the duration of the call; `sa`
        // outlives it; on success both handles are owned by us exactly once.
        let ok = unsafe { CreatePipe(&mut read, &mut write, &sa, 0) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: handles returned by a successful CreatePipe are valid and
        // unowned elsewhere.
        unsafe {
            Ok((
                OwnedHandle::from_raw_handle(read as _),
                OwnedHandle::from_raw_handle(write as _),
            ))
        }
    }

    fn make_inheritable(h: &OwnedHandle) -> io::Result<()> {
        // SAFETY: `h` is a valid open handle for the duration of the call.
        let ok = unsafe {
            SetHandleInformation(
                h.as_raw_handle() as HANDLE,
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Create the two pipes. Returns the parent transport and the child ends.
    pub fn create_pair() -> io::Result<(PipeTransport, ChildFds)> {
        // pipe A: parent → child
        let (child_read, to_child) = create_pipe()?;
        // pipe B: child → parent
        let (from_child, child_write) = create_pipe()?;
        make_inheritable(&child_read)?;
        make_inheritable(&child_write)?;
        let reader = tokio::fs::File::from_std(std::fs::File::from(from_child));
        let writer = tokio::fs::File::from_std(std::fs::File::from(to_child));
        let transport = PipeTransport::from_streams(Box::new(reader), Box::new(writer));
        Ok((
            transport,
            ChildFds {
                read: child_read,
                write: child_write,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn recv_reassembles_split_and_coalesced_frames() {
        let (ours, mut theirs) = duplex(64);
        let (rd, wr) = tokio::io::split(ours);
        let mut t = PipeTransport::from_streams(Box::new(rd), Box::new(wr));

        // Two messages in one write, the second one cut in half.
        let mut bytes = Vec::new();
        encode_frame(br#"{"id":1}"#, &mut bytes);
        encode_frame(br#"{"id":2,"result":{}}"#, &mut bytes);
        let cut = bytes.len() - 6;
        theirs.write_all(&bytes[..cut]).await.unwrap();
        assert_eq!(t.recv().await.unwrap().unwrap(), br#"{"id":1}"#);
        // Second frame is incomplete: recv must wait, not return garbage.
        let pending = tokio::time::timeout(std::time::Duration::from_millis(20), t.recv()).await;
        assert!(pending.is_err(), "recv returned before the NUL arrived");
        theirs.write_all(&bytes[cut..]).await.unwrap();
        assert_eq!(t.recv().await.unwrap().unwrap(), br#"{"id":2,"result":{}}"#);
        // EOF.
        drop(theirs);
        assert!(t.recv().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn send_appends_nul() {
        let (ours, mut theirs) = duplex(64);
        let (rd, wr) = tokio::io::split(ours);
        let mut t = PipeTransport::from_streams(Box::new(rd), Box::new(wr));
        t.send(br#"{"id":7}"#).await.unwrap();
        let mut got = vec![0u8; 9];
        theirs.read_exact(&mut got).await.unwrap();
        assert_eq!(got, b"{\"id\":7}\0");
    }

    /// Real OS pipes through a real child process (`cat`), no Chrome needed.
    /// Exercises `create_pair`, blocking child ends and EOF on child exit.
    #[cfg(unix)]
    #[tokio::test]
    async fn create_pair_roundtrips_through_cat() {
        let (mut transport, child) = create_pair().unwrap();
        let mut cat = tokio::process::Command::new("cat")
            .stdin(std::process::Stdio::from(child.read))
            .stdout(std::process::Stdio::from(child.write))
            .spawn()
            .expect("spawn cat");
        // Parent's copies of the child ends were moved into Stdio and closed
        // by spawn; nothing else to drop.
        transport
            .send(br#"{"id":1,"method":"Browser.getVersion"}"#)
            .await
            .unwrap();
        transport.send(br#"{"id":2}"#).await.unwrap();
        assert_eq!(
            transport.recv().await.unwrap().unwrap(),
            br#"{"id":1,"method":"Browser.getVersion"}"#
        );
        assert_eq!(transport.recv().await.unwrap().unwrap(), br#"{"id":2}"#);
        // Closing our write end makes cat exit → EOF on our read end.
        transport.writer = Box::new(tokio::io::sink());
        assert!(transport.recv().await.unwrap().is_none());
        assert!(cat.wait().await.unwrap().success());
    }
}
