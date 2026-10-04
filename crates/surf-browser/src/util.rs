//! Small helpers: base64 decoding (screenshots / PDFs arrive base64-encoded
//! over JSON) and random identifiers for isolated-world helper names.

use std::sync::atomic::{AtomicU64, Ordering};

/// Decode standard (or URL-safe) base64, ignoring whitespace and padding.
pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    fn val(c: u8) -> Result<Option<u8>, String> {
        Ok(Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' | b'\n' | b'\r' | b' ' | b'\t' => return Ok(None),
            _ => return Err(format!("invalid base64 byte {c:#04x}")),
        }))
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for &c in s.as_bytes() {
        let Some(v) = val(c)? else { continue };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A random-looking identifier of `len` alphanumeric characters, starting
/// with a letter. Seeded from the clock, the address of a stack slot and a
/// process-wide counter — enough so a page cannot guess the helper names
/// Surf installs in its isolated world, which is all that is needed.
pub fn random_ident(len: usize) -> String {
    const ALPHA: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";
    const ALNUM: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let salt = 0u8;
    let addr = std::ptr::addr_of!(salt) as u64;
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut x = now ^ addr.rotate_left(17) ^ n.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    if x == 0 {
        x = 0x2545_F491_4F6C_DD1D;
    }
    let mut next = move || {
        // xorshift64*
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let mut s = String::with_capacity(len);
    for i in 0..len {
        let r = next();
        let set = if i == 0 { ALPHA } else { ALNUM };
        s.push(set[(r >> 33) as usize % set.len()] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_known_vectors() {
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(base64_decode("Zm8=").unwrap(), b"fo");
        assert_eq!(base64_decode("Zm9v").unwrap(), b"foo");
        assert_eq!(base64_decode("Zm9vYg==").unwrap(), b"foob");
        assert_eq!(base64_decode("Zm9vYmE=").unwrap(), b"fooba");
        assert_eq!(base64_decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(base64_decode("Zm9v\nYmFy").unwrap(), b"foobar");
        assert_eq!(
            base64_decode("iVBORw0KGgo=").unwrap(),
            [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n']
        );
        assert!(base64_decode("Zm9v!").is_err());
    }

    #[test]
    fn random_idents_are_distinct_identifiers() {
        let a = random_ident(12);
        let b = random_ident(12);
        assert_eq!(a.len(), 12);
        assert_ne!(a, b);
        assert!(a.chars().next().unwrap().is_ascii_alphabetic());
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
