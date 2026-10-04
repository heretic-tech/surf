//! Typed commands.
//!
//! Decision 11: structs for an allow-list of domains are generated from the
//! vendored `protocol/browser_protocol.json` + `protocol/js_protocol.json`
//! (see `protocol/VERSION`) into [`generated`]. Everything outside the
//! allow-list goes through [`crate::Session::call_raw`].
//!
//! Planned allow-list (task 2): `Target`, `Page`, `Runtime` (minus `enable`),
//! `Input`, `DOM` (getContentQuads / scrollIntoViewIfNeeded only), `Network`,
//! `Fetch`, `Storage`, `Emulation`, `Browser`.

pub mod generated;

use serde::de::DeserializeOwned;
use serde::Serialize;

/// A typed CDP command: its parameters serialise to the `params` object and
/// `METHOD` is the `Domain.method` string.
pub trait Command: Serialize {
    /// `Domain.method`.
    const METHOD: &'static str;
    /// Shape of the `result` object.
    type Response: DeserializeOwned;
}
