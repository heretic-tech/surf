//! `on element_appears(selector)`: a `MutationObserver` installed in the
//! isolated world under a random name that calls a `Runtime.addBinding`
//! binding when a match appears; the runtime receives
//! `Runtime.bindingCalled` and runs the handler body. Never `DOM.enable`.
//! Implemented in task 8.

/// Identifier of one installed observer (for removal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ObserverId(pub u64);
