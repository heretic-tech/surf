//! `surf_browser::BrowserError` → `surf_vm::RuntimeError`: the message the
//! script sees, plus `selector` / `cdp_method` for the diagnostic. The
//! original error stays attached as the cause so the CLI can tell "no
//! browser found" (exit 3) from any other failure.

use surf_browser::BrowserError;
use surf_cdp::CdpError;
use surf_vm::RuntimeError;

/// Convert a browser failure raised by `action` (`click`, `goto`, …).
pub fn convert(e: BrowserError, action: &str) -> RuntimeError {
    let (message, selector, method) = match &e {
        BrowserError::Timeout { selector, .. } => (e.to_string(), selector.clone(), None),
        BrowserError::Element { message, selector } => {
            (message.clone(), Some(selector.clone()), None)
        }
        BrowserError::Ambiguous { names } => (ambiguous_message(names, action), None, None),
        BrowserError::Cdp(CdpError::Protocol {
            method, message, ..
        }) => (format!("{action}: {message}"), None, Some(method.clone())),
        BrowserError::Cdp(CdpError::Timeout { method }) => (
            format!("{action}: the browser did not answer {method} in time"),
            None,
            Some(method.clone()),
        ),
        BrowserError::Cdp(CdpError::BrowserCrashed {
            exit_code,
            stderr_tail,
        }) => {
            let mut m = match exit_code {
                Some(c) => format!("the browser crashed during {action} (exit code {c})"),
                None => format!("the browser crashed during {action}"),
            };
            let tail = stderr_tail.trim();
            if !tail.is_empty() {
                m.push_str("\nbrowser stderr:\n");
                m.push_str(tail);
            }
            (m, None, None)
        }
        BrowserError::Cdp(CdpError::Closed) => (
            format!("{action}: the browser connection is closed"),
            None,
            None,
        ),
        BrowserError::Script { .. } => (format!("{action}: {e}"), None, None),
        BrowserError::PageClosed { .. } | BrowserError::Navigation { .. } => {
            (format!("{action}: {e}"), None, None)
        }
        _ => (e.to_string(), None, None),
    };
    let mut r = RuntimeError::new(message).with_cause(e);
    if let Some(s) = selector {
        r = r.with_selector(s);
    }
    if let Some(m) = method {
        r = r.with_cdp_method(m);
    }
    r
}

/// `several pages are open (1, 2, "login") — say which: page(2).click(…)`.
pub fn ambiguous_message(names: &[String], action: &str) -> String {
    format!(
        "several pages are open ({}) — say which: page(2).{action}(…)",
        names.join(", ")
    )
}

/// Whether the error is "no Chrome/Chromium found" — discovery failed, or
/// the explicit `browser.path` / `SURF_CHROME` is not a binary (CLI exit
/// code 3).
pub fn is_no_browser(e: &RuntimeError) -> bool {
    e.cause
        .as_deref()
        .and_then(|c| c.downcast_ref::<BrowserError>())
        .is_some_and(|b| match b {
            BrowserError::NotFound { .. } => true,
            BrowserError::Config { what, .. } => what == "browser.path" || what == "SURF_CHROME",
            _ => false,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambiguity_names_the_fix() {
        let e = convert(
            BrowserError::Ambiguous {
                names: vec!["1".into(), "2".into(), "\"login\"".into()],
            },
            "click",
        );
        assert_eq!(
            e.message,
            "several pages are open (1, 2, \"login\") — say which: page(2).click(…)"
        );
    }

    #[test]
    fn timeout_carries_selector_and_protocol_carries_method() {
        let e = convert(
            BrowserError::Timeout {
                action: "click".into(),
                selector: Some("#go".into()),
                waited_ms: 5000,
                last_state: Some("hidden".into()),
            },
            "click",
        );
        assert_eq!(e.selector.as_deref(), Some("#go"));
        assert!(e.message.contains("timed out after 5.0s"));
        let e = convert(
            BrowserError::Cdp(CdpError::Protocol {
                code: -32000,
                message: "No node".into(),
                method: "DOM.getContentQuads".into(),
            }),
            "hover",
        );
        assert_eq!(e.cdp_method.as_deref(), Some("DOM.getContentQuads"));
        assert!(!is_no_browser(&e));
    }

    #[test]
    fn crash_includes_stderr_tail_and_not_found_is_detected() {
        let e = convert(
            BrowserError::Cdp(CdpError::BrowserCrashed {
                exit_code: Some(9),
                stderr_tail: "boom\n".into(),
            }),
            "goto",
        );
        assert!(e.message.contains("exit code 9"));
        assert!(e.message.ends_with("browser stderr:\nboom"));
        let e = convert(BrowserError::NotFound { tried: vec![] }, "goto");
        assert!(is_no_browser(&e));
    }
}
