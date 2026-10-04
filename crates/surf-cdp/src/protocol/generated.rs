//! Generated typed commands (placeholder).
//!
//! Task 2 replaces this file with output of the protocol codegen
//! (`cargo xtask codegen` or a `build.rs` reading `protocol/*.json`). Until
//! then it holds hand-written structs for the handful of calls the scaffold
//! references.

use super::Command;
use serde::{Deserialize, Serialize};

/// `Target.getTargets`
#[derive(Debug, Clone, Default, Serialize)]
pub struct GetTargets {}

/// One entry of `Target.getTargets`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TargetInfo {
    /// Target id.
    pub target_id: String,
    /// `page`, `iframe`, `service_worker`, …
    #[serde(rename = "type")]
    pub kind: String,
    /// Page title.
    pub title: String,
    /// Current URL.
    pub url: String,
    /// Whether a client is attached.
    pub attached: bool,
    /// Browser context the target lives in.
    #[serde(default)]
    pub browser_context_id: Option<String>,
}

/// Response of `Target.getTargets`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetTargetsResponse {
    /// All targets.
    pub target_infos: Vec<TargetInfo>,
}

impl Command for GetTargets {
    const METHOD: &'static str = "Target.getTargets";
    type Response = GetTargetsResponse;
}
