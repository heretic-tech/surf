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
use crate::selector::{Selector, RESOLVER_SOURCE};
use crate::util::random_ident;
use serde_json::Value;
use surf_cdp::protocol::runtime;
use surf_cdp::Session;

/// Length of the random world and helper names.
const NAME_LEN: usize = 12;

/// Object group every handle Surf creates belongs to, so a world can drop
/// them all at once with `Runtime.releaseObjectGroup`.
const OBJECT_GROUP: &str = "surf";

/// A handle to one isolated world on one frame. Cheap to clone.
#[derive(Clone, Debug)]
pub struct World {
    /// Session.
    pub session: Session,
    /// `executionContextId` of the world.
    pub context_id: i64,
    /// Random world name (12 alphanumeric characters).
    pub name: String,
    /// Frame the world lives on.
    pub frame_id: String,
    /// Name of the selector resolver installed on the world's global
    /// object. Random per world; the main world never sees it.
    pub resolver: String,
}

/// Does this error mean the execution context is gone (navigation, frame
/// detach, target closed)? The page then drops its world and creates a new
/// one once.
pub fn context_lost(e: &BrowserError) -> bool {
    e.protocol_message_contains("Cannot find context")
        || e.protocol_message_contains("Execution context was destroyed")
        || e.protocol_message_contains("Inspected target navigated or closed")
        || e.protocol_message_contains("Cannot find default execution context")
}

impl World {
    /// `Page.createIsolatedWorld{frameId, worldName, grantUniveralAccess}`
    /// then install the selector resolver under a random name.
    pub async fn create(session: Session, frame_id: &str) -> Result<World, BrowserError> {
        let name = random_ident(NAME_LEN);
        let created = session
            .send(surf_cdp::protocol::page::CreateIsolatedWorld {
                frame_id: frame_id.to_owned(),
                world_name: Some(name.clone()),
                grant_univeral_access: Some(true),
                content_security_policy: None,
            })
            .await?;
        let world = World {
            session,
            context_id: created.execution_context_id,
            name,
            frame_id: frame_id.to_owned(),
            resolver: format!("__surf_{}", random_ident(NAME_LEN)),
        };
        world.install_resolver().await?;
        tracing::debug!(
            "isolated world {} (context {}) on frame {}",
            world.name,
            world.context_id,
            world.frame_id
        );
        Ok(world)
    }

    async fn install_resolver(&self) -> Result<(), BrowserError> {
        // `this` is the world's global object when only
        // `executionContextId` is given.
        let decl = format!(
            "function() {{ this[{:?}] = {RESOLVER_SOURCE}; }}",
            self.resolver
        );
        self.call(&decl, vec![]).await.map(|_| ())
    }

    /// Evaluate a JS expression (`Runtime.evaluate{contextId}`); promises
    /// are awaited; the result is returned by value (`undefined` → `null`).
    pub async fn eval(&self, expression: &str) -> Result<Value, BrowserError> {
        let r = self
            .session
            .send(runtime::Evaluate {
                expression: expression.to_owned(),
                context_id: Some(self.context_id),
                return_by_value: Some(true),
                await_promise: Some(true),
                object_group: Some(OBJECT_GROUP.into()),
                ..Default::default()
            })
            .await?;
        if let Some(ex) = r.exception_details {
            return Err(script_error(ex));
        }
        Ok(remote_value(r.result))
    }

    /// Call `function_declaration` with JSON arguments; the result is
    /// returned by value. `this` is the global object.
    pub async fn call(
        &self,
        function_declaration: &str,
        args: Vec<Value>,
    ) -> Result<Value, BrowserError> {
        self.call_args(None, function_declaration, by_value(args), true)
            .await
            .map(remote_value)
    }

    /// Call `function_declaration` and keep the result as a handle
    /// (`RemoteObject` with `objectId` for objects; primitives still come
    /// back as values).
    pub async fn call_handle(
        &self,
        function_declaration: &str,
        args: Vec<runtime::CallArgument>,
    ) -> Result<runtime::RemoteObject, BrowserError> {
        self.call_args(None, function_declaration, args, false)
            .await
    }

    /// Call a function with `this` bound to the object behind `object_id`;
    /// result by value.
    pub async fn call_on(
        &self,
        object_id: &str,
        function_declaration: &str,
        args: Vec<Value>,
    ) -> Result<Value, BrowserError> {
        self.call_args(Some(object_id), function_declaration, by_value(args), true)
            .await
            .map(remote_value)
    }

    /// Call a function with `this` bound to `object_id`; result as a handle.
    pub async fn call_on_handle(
        &self,
        object_id: &str,
        function_declaration: &str,
        args: Vec<runtime::CallArgument>,
    ) -> Result<runtime::RemoteObject, BrowserError> {
        self.call_args(Some(object_id), function_declaration, args, false)
            .await
    }

    async fn call_args(
        &self,
        object_id: Option<&str>,
        function_declaration: &str,
        args: Vec<runtime::CallArgument>,
        by_value: bool,
    ) -> Result<runtime::RemoteObject, BrowserError> {
        let r = self
            .session
            .send(runtime::CallFunctionOn {
                function_declaration: function_declaration.to_owned(),
                object_id: object_id.map(str::to_owned),
                arguments: Some(args),
                return_by_value: Some(by_value),
                await_promise: Some(true),
                execution_context_id: if object_id.is_none() {
                    Some(self.context_id)
                } else {
                    None
                },
                object_group: Some(OBJECT_GROUP.into()),
                ..Default::default()
            })
            .await?;
        if let Some(ex) = r.exception_details {
            return Err(script_error(ex));
        }
        Ok(r.result)
    }

    /// Resolve a selector to element handles (`objectId`s). `all = false`
    /// returns at most one.
    pub async fn resolve(&self, sel: &Selector, all: bool) -> Result<Vec<String>, BrowserError> {
        let (kind, text) = sel.parts();
        let decl = format!(
            "function(k, s, a) {{ const r = this[{:?}](k, s, a); return a ? r : (r ? [r] : []); }}",
            self.resolver
        );
        let list = self
            .call_handle(
                &decl,
                by_value(vec![Value::from(kind), Value::from(text), Value::from(all)]),
            )
            .await?;
        let Some(list_id) = list.object_id else {
            return Ok(Vec::new());
        };
        // One `Runtime.getProperties` yields a handle per array element.
        let props = self
            .session
            .send(runtime::GetProperties {
                object_id: list_id.clone(),
                own_properties: Some(true),
                ..Default::default()
            })
            .await?;
        let mut indexed: Vec<(usize, String)> = props
            .result
            .into_iter()
            .filter_map(|p| {
                let i: usize = p.name.parse().ok()?;
                Some((i, p.value?.object_id?))
            })
            .collect();
        indexed.sort_by_key(|(i, _)| *i);
        self.release(&list_id).await;
        Ok(indexed.into_iter().map(|(_, id)| id).collect())
    }

    /// `Runtime.releaseObject` (best effort).
    pub async fn release(&self, object_id: &str) {
        let _ = self
            .session
            .send(runtime::ReleaseObject {
                object_id: object_id.to_owned(),
            })
            .await;
    }

    /// Release every handle this world handed out (best effort).
    pub async fn release_all(&self) {
        let _ = self
            .session
            .send(runtime::ReleaseObjectGroup {
                object_group: OBJECT_GROUP.into(),
            })
            .await;
    }

    /// `Runtime.addBinding{name, executionContextId}`: calling
    /// `globalThis[name](payload)` in this world raises
    /// `Runtime.bindingCalled` — without `Runtime.enable`.
    pub async fn add_binding(&self, name: &str) -> Result<(), BrowserError> {
        self.session
            .send(runtime::AddBinding {
                name: name.to_owned(),
                execution_context_id: Some(self.context_id),
                execution_context_name: None,
            })
            .await?;
        Ok(())
    }
}

/// Wrap JSON values as `CallArgument`s.
pub fn by_value(args: Vec<Value>) -> Vec<runtime::CallArgument> {
    args.into_iter()
        .map(|v| runtime::CallArgument {
            value: Some(v),
            unserializable_value: None,
            object_id: None,
        })
        .collect()
}

/// A `CallArgument` referring to a handle.
pub fn by_handle(object_id: &str) -> runtime::CallArgument {
    runtime::CallArgument {
        value: None,
        unserializable_value: None,
        object_id: Some(object_id.to_owned()),
    }
}

/// The JSON value of a by-value `RemoteObject` (`undefined`, `NaN`, … →
/// `null`).
pub fn remote_value(r: runtime::RemoteObject) -> Value {
    r.value.unwrap_or(Value::Null)
}

fn script_error(ex: runtime::ExceptionDetails) -> BrowserError {
    let text = ex
        .exception
        .as_ref()
        .and_then(|e| {
            e.description
                .as_ref()
                .and_then(|d| d.lines().next().map(str::to_owned))
                .or_else(|| e.value.as_ref().map(value_to_string))
        })
        .unwrap_or_else(|| ex.text.clone());
    BrowserError::Script {
        text,
        line: Some(ex.line_number),
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}
