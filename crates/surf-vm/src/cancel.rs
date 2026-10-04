//! Cooperative cancellation.

use std::cell::Cell;
use std::rc::Rc;

/// A clonable flag the runtime flips on `exit` / supervisor restart. The VM
/// checks it at loop back-edges and before every host call, and races it
/// against `Host::sleep`.
#[derive(Clone, Default, Debug)]
pub struct CancelToken(Rc<Cell<bool>>);

impl CancelToken {
    /// New, un-cancelled token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation.
    pub fn cancel(&self) {
        self.0.set(true);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.get()
    }
}
