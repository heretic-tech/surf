//! Structured diagnostics rendered with `ariadne`.
//!
//! [`Diagnostic`] is shared by every stage: the lexer and parser emit syntax
//! errors, `surf-vm` emits compile errors, and the runtime converts a
//! `RuntimeError` (span + selector + CDP method) into one via
//! [`Diagnostic::runtime`] so every error the CLI prints looks the same.

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
    /// Free-form help text (`help: …`).
    pub help: Option<String>,
    /// Free-form note (`note: …`), e.g. the CDP method of a runtime error.
    pub note: Option<String>,
}

impl Diagnostic {
    /// Build a diagnostic of `severity` at `span`.
    pub fn new(severity: Severity, message: impl Into<String>, span: Span) -> Self {
        Self {
            severity,
            message: message.into(),
            span,
            label: None,
            secondary: Vec::new(),
            help: None,
            note: None,
        }
    }

    /// Build an error at `span`.
    pub fn error(message: impl Into<String>, span: Span) -> Self {
        Self::new(Severity::Error, message, span)
    }

    /// Build a warning at `span`.
    pub fn warning(message: impl Into<String>, span: Span) -> Self {
        Self::new(Severity::Warning, message, span)
    }

    /// Build the diagnostic for a runtime error, in the shape the CLI
    /// prints: `message (selector: #x) [CDP.method]` with the method as a
    /// note. `span` is `None` when the failing operation has no source
    /// location (the diagnostic then renders without a code excerpt).
    pub fn runtime(
        message: impl Into<String>,
        span: Option<Span>,
        selector: Option<&str>,
        cdp_method: Option<&str>,
    ) -> Self {
        let mut text = message.into();
        if let Some(sel) = selector {
            text.push_str(&format!(" (selector: {sel})"));
        }
        if let Some(method) = cdp_method {
            text.push_str(&format!(" [{method}]"));
        }
        let mut d = Self::error(text, span.unwrap_or_default());
        d.label = selector.map(|s| format!("selector: {s}"));
        if let Some(method) = cdp_method {
            d.note = Some(format!("CDP method: {method}"));
        }
        d
    }

    /// Attach a primary label.
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Attach a secondary label.
    pub fn with_secondary(mut self, span: Span, label: impl Into<String>) -> Self {
        self.secondary.push((span, label.into()));
        self
    }

    /// Attach help text.
    pub fn with_help(mut self, help: impl Into<String>) -> Self {
        self.help = Some(help.into());
        self
    }

    /// Attach help text when `help` is `Some`.
    pub fn with_help_opt(mut self, help: Option<String>) -> Self {
        self.help = help;
        self
    }

    /// Attach a note.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    /// Render this diagnostic alone (see [`Diagnostics::render`]).
    pub fn render(&self, name: &str, source: &str, color: bool) -> String {
        use ariadne::{Color, Config, IndexType, Label, Report, ReportKind, Source};
        let kind = match self.severity {
            Severity::Error => ReportKind::Custom("error", Color::Red),
            Severity::Warning => ReportKind::Custom("warning", Color::Yellow),
        };
        let clamp = |s: &Span| {
            let start = (s.start as usize).min(source.len());
            let end = (s.end as usize).clamp(start, source.len());
            start..end
        };
        let mut report = Report::build(kind, (name, clamp(&self.span)))
            .with_config(
                Config::default()
                    .with_color(color)
                    .with_index_type(IndexType::Byte),
            )
            .with_message(&self.message);
        // ariadne draws no marker for a label without a message.
        let label = Label::new((name, clamp(&self.span)))
            .with_order(0)
            .with_message(self.label.as_deref().unwrap_or("here"));
        report = report.with_label(label);
        for (i, (span, text)) in self.secondary.iter().enumerate() {
            report = report.with_label(
                Label::new((name, clamp(span)))
                    .with_message(text)
                    .with_order(i as i32 + 1),
            );
        }
        if let Some(help) = &self.help {
            report = report.with_help(help);
        }
        if let Some(note) = &self.note {
            report = report.with_note(note);
        }
        let mut out = Vec::new();
        let _ = report
            .finish()
            .write((name, Source::from(source)), &mut out);
        String::from_utf8_lossy(&out).into_owned()
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

    /// Add a diagnostic.
    pub fn push(&mut self, d: Diagnostic) {
        self.items.push(d);
    }

    /// Append every item of `other`.
    pub fn extend(&mut self, other: Diagnostics) {
        self.items.extend(other.items);
    }

    /// Whether any item is an error.
    pub fn has_errors(&self) -> bool {
        self.items.iter().any(|d| d.severity == Severity::Error)
    }

    /// Whether the batch is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Number of diagnostics.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Render all diagnostics with `ariadne` into a string (ANSI colours
    /// when `color` is true).
    pub fn render(&self, source: &str, color: bool) -> String {
        let mut out = String::new();
        for d in &self.items {
            out.push_str(&d.render(&self.name, source, color));
        }
        out
    }
}

impl fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for d in &self.items {
            writeln!(f, "{}:{}: {}", self.name, d.span.start, d.message)?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostics {}

/// Levenshtein edit distance between two strings (by `char`).
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Pick the candidate closest to `word`, if one is close enough to be a
/// plausible typo (distance ≤ 1 for short words, ≤ 2 up to 8 characters,
/// ≤ 3 beyond). Ties go to the earlier candidate.
pub fn suggest<'a>(word: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let len = word.chars().count();
    let max = match len {
        0..=4 => 1,
        5..=8 => 2,
        _ => 3,
    };
    let mut best: Option<(usize, &'a str)> = None;
    for cand in candidates {
        if cand == word {
            continue;
        }
        let d = edit_distance(word, cand);
        if d <= max && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, cand));
        }
    }
    best.map(|(_, c)| c)
}

/// `did you mean `x`?` help text for `word` against `candidates`.
pub fn did_you_mean<'a>(
    word: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    suggest(word, candidates).map(|c| format!("did you mean `{c}`?"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("whlie", "while"), 2);
    }

    #[test]
    fn suggestions() {
        assert_eq!(
            suggest("whlie", crate::token::KEYWORDS.iter().copied()),
            Some("while")
        );
        assert_eq!(suggest("headles", ["headless", "path"]), Some("headless"));
        assert_eq!(suggest("zzzzzz", ["headless", "path"]), None);
        assert_eq!(suggest("on", ["in"]), Some("in"));
    }
}
