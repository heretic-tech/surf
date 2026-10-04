//! Isolated worlds — the only place scripts' JavaScript runs.
//!
//! `Page.createIsolatedWorld{frameId, worldName, grantUniveralAccess:true}`
//! → `executionContextId`; evaluation via
//! `Runtime.callFunctionOn{executionContextId, functionDeclaration,
//! arguments, returnByValue:true, awaitPromise:true}`. Helper functions
//! (selector resolver, mutation observer) are installed under random names.
//! The world → runtime channel is `Runtime.addBinding{executionContextId,
//! name}` + `Runtime.bindingCalled` (fires without `Runtime.enable`; verified
//! by an e2e test). Implemented in task 4.

use crate::error::BrowserError;
use surf_cdp::Session;

/// A handle to one isolated world on one frame.
#[derive(Clone, Debug)]
pub struct World {
    /// Session.
    pub session: Session,
    /// `executionContextId` of the world.
    pub context_id: i64,
    /// Random world name (`surf_<hex>`).
    pub name: String,
}

impl World {
    /// Create the world on `frame_id`.
    pub async fn create(session: Session, frame_id: &str) -> Result<World, BrowserError> {
        let _ = (session, frame_id);
        Err(BrowserError::Unsupported(
            "World::create not implemented yet (task 4)".into(),
        ))
    }

    /// Evaluate a JS expression; returns the JSON value.
    pub async fn eval(&self, expression: &str) -> Result<serde_json::Value, BrowserError> {
        let _ = expression;
        Err(BrowserError::Unsupported(
            "World::eval not implemented yet (task 4)".into(),
        ))
    }

    /// Call a function declaration with JSON arguments.
    pub async fn call(
        &self,
        function_declaration: &str,
        args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, BrowserError> {
        let _ = (function_declaration, args);
        Err(BrowserError::Unsupported(
            "World::call not implemented yet (task 4)".into(),
        ))
    }
}
