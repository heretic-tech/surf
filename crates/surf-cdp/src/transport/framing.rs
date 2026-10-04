//! `\0`-delimited framing used by `--remote-debugging-pipe`.
//!
//! Each message is the UTF-8 JSON text followed by exactly one NUL byte.
//! (`--remote-debugging-pipe=cbor` exists but is not used in v0.1 — see
//! TASKS.md.)

/// Append `json` plus the NUL terminator to `out`.
pub fn encode_frame(json: &[u8], out: &mut Vec<u8>) {
    out.reserve(json.len() + 1);
    out.extend_from_slice(json);
    out.push(0);
}

/// Incremental decoder: feed bytes, pull complete frames.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    scanned: usize,
}

impl FrameDecoder {
    /// Empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes from the pipe.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Pop the next complete frame (without the NUL), if one is buffered.
    pub fn next_frame(&mut self) -> Option<Vec<u8>> {
        let rel = self.buf[self.scanned..].iter().position(|&b| b == 0)?;
        let end = self.scanned + rel;
        let frame = self.buf[..end].to_vec();
        self.buf.drain(..=end);
        self.scanned = 0;
        Some(frame)
    }

    /// Bytes buffered but not yet forming a frame.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_split_frames() {
        let mut out = Vec::new();
        encode_frame(br#"{"id":1}"#, &mut out);
        encode_frame(br#"{"id":2}"#, &mut out);
        let mut dec = FrameDecoder::new();
        dec.feed(&out[..5]);
        assert!(dec.next_frame().is_none());
        dec.feed(&out[5..]);
        assert_eq!(dec.next_frame().unwrap(), br#"{"id":1}"#);
        assert_eq!(dec.next_frame().unwrap(), br#"{"id":2}"#);
        assert!(dec.next_frame().is_none());
        assert_eq!(dec.pending(), 0);
    }
}
