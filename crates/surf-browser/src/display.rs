//! Linux `virtual: true`: start an Xvfb server on a free display, export
//! `DISPLAY` to the Chrome child only, and tear it down on shutdown. On
//! non-Linux platforms `virtual: true` is accepted and ignored with a
//! warning. Implemented in task 3.

use crate::error::BrowserError;

/// A running Xvfb server.
#[derive(Debug)]
pub struct Xvfb {
    display: String,
}

impl Xvfb {
    /// Start `Xvfb :N -screen 0 WxHx24` on a free display number.
    pub async fn start(width: u32, height: u32) -> Result<Self, BrowserError> {
        let _ = (width, height);
        Err(BrowserError::Unsupported(
            "virtual display not implemented yet (task 3)".into(),
        ))
    }

    /// The `DISPLAY` value (e.g. `:99`).
    pub fn display(&self) -> &str {
        &self.display
    }
}
