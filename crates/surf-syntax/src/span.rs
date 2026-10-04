//! Byte-offset source spans.

use serde::{Deserialize, Serialize};

/// A half-open byte range `[start, end)` into the source text.
///
/// Spans are file-relative; the file name travels with
/// [`crate::Diagnostics`], not with each span.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Span {
    /// Start byte offset (inclusive).
    pub start: u32,
    /// End byte offset (exclusive).
    pub end: u32,
}

impl Span {
    /// Build a span from byte offsets.
    pub const fn new(start: u32, end: u32) -> Self {
        Self { start, end }
    }

    /// Smallest span covering both `self` and `other`.
    pub fn merge(self, other: Span) -> Span {
        Span {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    /// Length in bytes.
    pub fn len(&self) -> usize {
        (self.end - self.start) as usize
    }

    /// Whether the span is empty.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// `start..end` as a `Range<usize>` for slicing source text.
    pub fn range(&self) -> std::ops::Range<usize> {
        self.start as usize..self.end as usize
    }

    /// Empty span at `offset`.
    pub const fn at(offset: u32) -> Self {
        Self {
            start: offset,
            end: offset,
        }
    }

    /// 1-based `(line, column)` of the span start inside `source`
    /// (column counts characters, not bytes).
    pub fn line_col(&self, source: &str) -> (usize, usize) {
        line_col(source, self.start as usize)
    }

    /// The source text covered by this span (clamped to the source length).
    pub fn slice<'a>(&self, source: &'a str) -> &'a str {
        let start = (self.start as usize).min(source.len());
        let end = (self.end as usize).clamp(start, source.len());
        &source[start..end]
    }
}

/// 1-based `(line, column)` of byte `offset` inside `source`.
///
/// Offsets past the end report the position just after the last character.
pub fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let offset = offset.min(source.len());
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map(|i| i + 1).unwrap_or(0);
    let col = before[line_start..].chars().count() + 1;
    (line, col)
}

/// A value with its source span.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spanned<T> {
    /// The wrapped node.
    pub node: T,
    /// Where it came from.
    pub span: Span,
}

impl<T> Spanned<T> {
    /// Wrap `node` with `span`.
    pub fn new(node: T, span: Span) -> Self {
        Self { node, span }
    }
}
