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
