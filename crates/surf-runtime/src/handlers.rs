//! Reactive handler registration and dispatch. Implemented in task 8.
//!
//! | event              | source                                              |
//! |--------------------|-----------------------------------------------------|
//! | `element_appears`  | isolated-world MutationObserver → `Runtime.bindingCalled` |
//! | `navigation`       | `Page.frameNavigated` (main frame)                  |
//! | `dialog`           | `Page.javascriptDialogOpening`                      |
//! | `request`          | `Network.requestWillBeSent` (enables `Network`)     |
//! | `response`         | `Network.responseReceived` (enables `Network`)      |
//! | `message`          | actor mailbox                                        |

/// Handler kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HandlerEvent {
    /// `on element_appears(selector):`
    ElementAppears,
    /// `on navigation(pattern):`
    Navigation,
    /// `on dialog:`
    Dialog,
    /// `on request(pattern):`
    Request,
    /// `on response(pattern):`
    Response,
    /// `on message:` (actors only)
    Message,
}

impl HandlerEvent {
    /// Parse the event name used after `on`.
    pub fn parse(name: &str) -> Option<HandlerEvent> {
        Some(match name {
            "element_appears" => HandlerEvent::ElementAppears,
            "navigation" => HandlerEvent::Navigation,
            "dialog" => HandlerEvent::Dialog,
            "request" => HandlerEvent::Request,
            "response" => HandlerEvent::Response,
            "message" => HandlerEvent::Message,
            _ => return None,
        })
    }
}
