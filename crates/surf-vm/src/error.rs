//! Runtime errors.

use std::fmt;
use surf_syntax::Span;

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
