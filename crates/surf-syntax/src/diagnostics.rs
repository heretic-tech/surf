//! Structured diagnostics rendered with `ariadne`.

use crate::span::Span;
use std::fmt;

/// Diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Hard error; parsing failed.
    Error,
    /// Warning; parsing succeeded.
    Warning,
}

/// One diagnostic message with a primary span and optional notes.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    /// Severity.
    pub severity: Severity,
    /// Human message, e.g. `expected an indented block after ':'`.
    pub message: String,
    /// Primary span (may be empty at EOF).
    pub span: Span,
    /// Label shown under the primary span.
    pub label: Option<String>,
    /// Extra `(span, text)` labels.
    pub secondary: Vec<(Span, String)>,
    /// Free-form help text.
    pub help: Option<String>,
}

impl Diagnostic {
    /// Build an error at `span`.
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Self {
            severity: Severity::Error,
            message: message.into(),
            span,
            label: None,
            secondary: Vec::new(),
            help: None,
        }
    }

    /// Attach a primary label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Attach help text.
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }
}

/// A batch of diagnostics for one source file.
#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostics {
    /// File name (for rendering).
    pub name: String,
    /// Diagnostics in emission order.
    pub items: Vec<Diagnostic>,
}

impl Diagnostics {
    /// Empty batch for `name`.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            items: Vec::new(),
        }
    }

    /// Single-error batch.
    pub fn single(name: impl Into<String>, d: Diagnostic) -> Self {
        Self {
            name: name.into(),
            items: vec![d],
        }
    }

    /// Placeholder used by scaffold stubs. Removed once the stage exists.
    pub fn unimplemented(name: &str, stage: &str) -> Self {
        Self::single(
            name,
            Diagnostic::error(format!("{stage} not implemented yet"), Span::default()),
        )
    }

    /// Add a diagnostic.
    pub fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }

    /// Whether any item is an error.
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Render all diagnostics with `ariadne` into a string (ANSI colours
    /// when `color` is true).
    pub fn render(&self, source: &str, color: bool) -> String {
        use ariadne::{Config, Label, Report, ReportKind, Source};
        let mut out = Vec::new();
        for d in &self.items {
            let kind = match d.severity {
                Severity::Error => ReportKind::Error,
                Severity::Warning => ReportKind::Warning,
            };
            let mut report = Report::build(kind, (self.name.as_str(), d.span.range()))
                .with_config(Config::default().with_color(color))
                .with_message(&d.message);
            let mut label = Label::new((self.name.as_str(), d.span.range()));
            if let Some(text) = &d.label {
                label = label.with_message(text);
            }
            report = report.with_label(label);
            for (span, text) in &d.secondary {
                report = report
                    .with_label(Label::new((self.name.as_str(), span.range())).with_message(text));
            }
            if let Some(help) = &d.help {
                report = report.with_help(help);
            }
            let _ = report
                .finish()
                .write((self.name.as_str(), Source::from(source)), &mut out);
        }
        String::from_utf8_lossy(&out).into_owned()
    }
}

impl fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for d in &self.items {
            writeln!(
                f,
                "{}:{}..{}: {}",
                self.name, d.span.start, d.span.end, d.message
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostics {}
