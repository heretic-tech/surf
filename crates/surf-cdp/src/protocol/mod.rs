//! Typed commands, responses and events.
//!
//! Decision 11: structs for an allow-list of domains are generated at build
//! time (`build.rs`) from the vendored `protocol/browser_protocol.json` +
//! `protocol/js_protocol.json` (pinned in `protocol/VERSION`). Everything
//! outside the allow-list goes through [`crate::Session::call_raw`].
//!
//! Allow-list: `Target`, `Page`, `Runtime`, `DOM`, `Input`, `Network`,
//! `Fetch`, `Emulation`, `Browser`, `Storage`, `IO`, `Security`.
//! `Runtime.enable` and `DOM.enable` are **not** generated (quiet rules).
//!
//! Shapes: every command `Domain.fooBar` becomes `domain::FooBar`
//! (`Serialize`, implements [`Command`] with `METHOD` and `Response`) and,
//! when it returns something, `domain::FooBarResponse` (`Deserialize`;
//! otherwise `Response = Empty`). Every event becomes `domain::FooBarEvent`
//! (`Deserialize`, implements [`ProtocolEvent`]). Named types become
//! aliases, structs or enums (enums carry a `#[serde(other)] Unknown`
//! variant so a newer Chrome cannot break deserialisation). Inline enums are
//! `String`; `any`, opaque objects and references to domains outside the
//! allow-list are `serde_json::Value`.
//!
//! ```no_run
//! # async fn demo(session: surf_cdp::Session) -> Result<(), surf_cdp::CdpError> {
//! use surf_cdp::protocol::target;
//! let r = session.send(target::CreateTarget {
//!     url: "about:blank".into(),
//!     ..Default::default()
//! }).await?;
//! println!("{}", r.target_id);
//! # Ok(()) }
//! ```

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// A typed CDP command: its parameters serialise to the `params` object and
/// `METHOD` is the `Domain.method` string.
pub trait Command: Serialize {
    /// `Domain.method`.
    const METHOD: &'static str;
    /// Shape of the `result` object.
    type Response: DeserializeOwned;
}

/// A typed CDP event payload; see [`crate::Event::parse`].
pub trait ProtocolEvent: DeserializeOwned {
    /// `Domain.event`.
    const METHOD: &'static str;
}

/// Response of commands that return nothing (`"result": {}`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub struct Empty {}

include!(concat!(env!("OUT_DIR"), "/protocol.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_params_serialise_with_protocol_names() {
        let c = target::AttachToTarget {
            target_id: "T1".into(),
            flatten: Some(true),
        };
        assert_eq!(target::AttachToTarget::METHOD, "Target.attachToTarget");
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            serde_json::json!({"targetId": "T1", "flatten": true})
        );
        // Optional fields are omitted, not null.
        let c = page::Navigate {
            url: "https://example.com".into(),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            serde_json::json!({"url": "https://example.com"})
        );
    }

    #[test]
    fn responses_and_events_deserialise() {
        let r: target::GetTargetsResponse = serde_json::from_value(serde_json::json!({
            "targetInfos": [{
                "targetId": "T1", "type": "page", "title": "t", "url": "about:blank",
                "attached": false, "canAccessOpener": false
            }]
        }))
        .unwrap();
        assert_eq!(r.target_infos[0].r#type, "page");
        let ev = crate::Event {
            method: "Page.frameNavigated".into(),
            params: serde_json::json!({
                "frame": {"id": "F", "loaderId": "L", "url": "about:blank", "domainAndRegistry": "",
                          "securityOrigin": "://", "mimeType": "text/html", "secureContextType": "Secure",
                          "crossOriginIsolatedContextType": "NotIsolated", "gatedAPIFeatures": []},
                "type": "Navigation"
            }),
            session_id: Some("S".into()),
        };
        let parsed: page::FrameNavigatedEvent = ev.parse().unwrap();
        assert_eq!(parsed.frame.id, "F");
        assert_eq!(parsed.r#type, page::NavigationType::Navigation);
        assert!(ev.parse::<page::FrameAttachedEvent>().is_err());
    }

    #[test]
    fn enums_round_trip_and_tolerate_unknown_values() {
        let v: network::ResourceType = serde_json::from_str("\"XHR\"").unwrap();
        assert_eq!(v, network::ResourceType::Xhr);
        assert_eq!(serde_json::to_string(&v).unwrap(), "\"XHR\"");
        let v: network::ResourceType = serde_json::from_str("\"FromTheFuture\"").unwrap();
        assert_eq!(v, network::ResourceType::Unrecognized);
    }

    #[test]
    fn recursive_types_are_boxed_and_cross_domain_refs_resolve() {
        let st = runtime::StackTrace {
            description: None,
            call_frames: vec![],
            parent: Some(Box::new(runtime::StackTrace {
                description: None,
                call_frames: vec![],
                parent: None,
                parent_id: None,
            })),
            parent_id: None,
        };
        assert!(st.parent.is_some());
        let _: Option<network::Cookie> = None;
        let _: Option<storage::GetCookiesResponse> = None;
    }

    #[test]
    fn forbidden_methods_have_no_typed_command() {
        // Compile-time: `runtime::Enable` / `dom::Enable` do not exist. Keep
        // the guard observable at runtime too.
        assert_eq!(crate::FORBIDDEN_METHODS, &["Runtime.enable", "DOM.enable"]);
    }
}
