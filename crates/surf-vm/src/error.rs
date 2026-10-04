//! Runtime errors.

use crate::value::Value;
use indexmap::IndexMap;
use std::fmt;
use std::rc::Rc;
use surf_syntax::{Diagnostic, Span};

/// Marker cause for `exit(code)`: propagates through every frame and is
/// not catchable by `try/catch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitRequest(pub i32);

impl fmt::Display for ExitRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "exit({})", self.0)
    }
}

impl std::error::Error for ExitRequest {}

/// Marker cause for cooperative cancellation (not catchable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl fmt::Display for Cancelled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// An error raised while running a script. Catchable with `try/catch`;
/// otherwise reported by the CLI with the span, selector and CDP method.
pub struct RuntimeError {
    /// Human message.
    pub message: String,
    /// Where in the script the failing operation was (if known).
    pub span: Option<Span>,
    /// CDP method that failed (e.g. `Input.dispatchMouseEvent`), if any.
    pub cdp_method: Option<String>,
    /// Selector involved (e.g. `#submit`), if any.
    pub selector: Option<String>,
    /// Underlying cause.
    pub cause: Option<Box<dyn std::error::Error>>,
}

impl RuntimeError {
    /// Message-only error.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            span: None,
            cdp_method: None,
            selector: None,
            cause: None,
        }
    }

    /// Attach a span.
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// Attach a CDP method.
    pub fn with_cdp_method(mut self, method: impl Into<String>) -> Self {
        self.cdp_method = Some(method.into());
        self
    }

    /// Attach a selector.
    pub fn with_selector(mut self, selector: impl Into<String>) -> Self {
        self.selector = Some(selector.into());
        self
    }

    /// Attach a cause.
    pub fn with_cause(mut self, cause: impl std::error::Error + 'static) -> Self {
        self.cause = Some(Box::new(cause));
        self
    }

    /// Attach a span only if none is set yet.
    pub fn or_span(mut self, span: Span) -> Self {
        if self.span.is_none() {
            self.span = Some(span);
        }
        self
    }

    /// The `exit(code)` pseudo-error.
    pub fn exit(code: i32) -> Self {
        RuntimeError::new(format!("exit({code})")).with_cause(ExitRequest(code))
    }

    /// The cancellation pseudo-error.
    pub fn cancelled() -> Self {
        RuntimeError::new("cancelled").with_cause(Cancelled)
    }

    /// `Some(code)` if this is an `exit` request.
    pub fn exit_code(&self) -> Option<i32> {
        self.cause
            .as_deref()
            .and_then(|c| c.downcast_ref::<ExitRequest>())
            .map(|e| e.0)
    }

    /// Whether this is the cancellation pseudo-error.
    pub fn is_cancelled(&self) -> bool {
        self.cause
            .as_deref()
            .map(|c| c.is::<Cancelled>())
            .unwrap_or(false)
    }

    /// Whether `try/catch` may catch this error (`exit` and cancellation
    /// are not catchable).
    pub fn is_catchable(&self) -> bool {
        self.exit_code().is_none() && !self.is_cancelled()
    }

    /// The diagnostic the CLI renders (`docs/language.md` § 10).
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::runtime(
            self.message.clone(),
            self.span,
            self.selector.as_deref(),
            self.cdp_method.as_deref(),
        )
    }

    /// The value bound by `catch e:` — a map with `message`, `selector`,
    /// `cdp_method` and `line` (`nil` when unknown).
    pub fn to_value(&self, line: Option<u32>) -> Value {
        let mut m: IndexMap<Rc<str>, Value> = IndexMap::new();
        m.insert(Rc::from("message"), Value::str(&self.message));
        m.insert(
            Rc::from("selector"),
            self.selector
                .as_deref()
                .map(Value::str)
                .unwrap_or(Value::Nil),
        );
        m.insert(
            Rc::from("cdp_method"),
            self.cdp_method
                .as_deref()
                .map(Value::str)
                .unwrap_or(Value::Nil),
        );
        m.insert(
            Rc::from("line"),
            line.map(|l| Value::Int(l as i64)).unwrap_or(Value::Nil),
        );
        Value::map(m)
    }
}

impl fmt::Debug for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeError")
            .field("message", &self.message)
            .field("span", &self.span)
            .field("cdp_method", &self.cdp_method)
            .field("selector", &self.selector)
            .field("cause", &self.cause.as_ref().map(|c| c.to_string()))
            .finish()
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)?;
        if let Some(sel) = &self.selector {
            write!(f, " (selector: {sel})")?;
        }
        if let Some(m) = &self.cdp_method {
            write!(f, " [{m}]")?;
        }
        Ok(())
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.cause.as_deref()
    }
}

impl From<String> for RuntimeError {
    fn from(s: String) -> Self {
        RuntimeError::new(s)
    }
}

impl From<&str> for RuntimeError {
    fn from(s: &str) -> Self {
        RuntimeError::new(s)
    }
}
