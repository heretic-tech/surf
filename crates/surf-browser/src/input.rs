//! Low-level input: `Input.dispatchMouseEvent`, `Input.dispatchKeyEvent`,
//! `Input.insertText`. Coordinates come from `DOM.getContentQuads` (works
//! without `DOM.enable`). Implemented in task 4.

/// Mouse button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    /// Left.
    Left,
    /// Right.
    Right,
    /// Middle.
    Middle,
}

impl MouseButton {
    /// CDP name.
    pub fn as_str(&self) -> &'static str {
        match self {
            MouseButton::Left => "left",
            MouseButton::Right => "right",
            MouseButton::Middle => "middle",
        }
    }
}
