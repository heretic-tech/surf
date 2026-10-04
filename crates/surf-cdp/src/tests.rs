//! Connection / session behaviour against the channel-backed fake transport.

use crate::connection::ALL_EVENTS;
use crate::transport::fake::{pair, FakeBrowser};
use crate::{CdpError, Connection};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

fn connect() -> (Arc<Connection>, FakeBrowser) {
    let (t, browser) = pair();
    (Connection::new(Box::new(t)), browser)
}

#[tokio::test]
async fn call_sends_id_method_params_and_returns_result() {
    let (conn, mut browser) = connect();
    let root = conn.root();
    let call = tokio::spawn(async move { root.call_raw("Browser.getVersion", json!({})).await });
    let sent = browser.next_sent().await;
    assert_eq!(
        sent,
        json!({"id": 1, "method": "Browser.getVersion", "params": {}})
    );
    browser.respond(1, json!({"product": "Chrome/1"}));
    assert_eq!(call.await.unwrap().unwrap(), json!({"product": "Chrome/1"}));
}

#[tokio::test]
async fn null_params_become_an_empty_object_and_ids_increase() {
    let (conn, mut browser) = connect();
    let root = conn.root();
    let r2 = root.clone();
    let a = tokio::spawn(async move { root.call_raw("A.a", serde_json::Value::Null).await });
    let b = tokio::spawn(async move { r2.call_raw("B.b", json!({"x": 1})).await });
    let first = browser.next_sent().await;
    let second = browser.next_sent().await;
    assert_eq!(first["params"], json!({}));
    assert_eq!(first["id"], 1);
    assert_eq!(second["id"], 2);
    assert_eq!(second["params"], json!({"x": 1}));
    // Answer out of order: correlation is by id, not arrival.
    browser.respond(2, json!({"b": true}));
    browser.respond(1, json!({"a": true}));
    assert_eq!(a.await.unwrap().unwrap(), json!({"a": true}));
    assert_eq!(b.await.unwrap().unwrap(), json!({"b": true}));
}

#[tokio::test]
async fn error_responses_map_to_protocol_errors_with_method() {
    let (conn, mut browser) = connect();
    let root = conn.root();
    let call = tokio::spawn(async move { root.call_raw("Page.navigate", json!({})).await });
    browser.next_sent().await;
    browser.fail(1, -32000, "Cannot navigate to invalid URL");
    match call.await.unwrap() {
        Err(CdpError::Protocol {
            code,
            message,
            method,
        }) => {
            assert_eq!(code, -32000);
            assert_eq!(message, "Cannot navigate to invalid URL");
            assert_eq!(method, "Page.navigate");
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }
}

#[tokio::test]
async fn events_interleave_with_responses() {
    let (conn, mut browser) = connect();
    let root = conn.root();
    let mut created = root.events("Target.targetCreated");
    let mut all = root.events(ALL_EVENTS);
    let call = tokio::spawn(async move { root.call_raw("Target.getTargets", json!({})).await });
    browser.next_sent().await;
    browser.event(
        "Target.targetCreated",
        json!({"targetInfo": {"targetId": "T1"}}),
        None,
    );
    browser.respond(1, json!({"targetInfos": []}));
    browser.event(
        "Target.targetCreated",
        json!({"targetInfo": {"targetId": "T2"}}),
        None,
    );
    assert_eq!(call.await.unwrap().unwrap(), json!({"targetInfos": []}));
    let e1 = crate::event::next(&mut created).await.unwrap();
    let e2 = crate::event::next(&mut created).await.unwrap();
    assert_eq!(e1.params["targetInfo"]["targetId"], "T1");
    assert_eq!(e2.params["targetInfo"]["targetId"], "T2");
    assert_eq!(e1.session_id, None);
    assert_eq!(all.recv().await.unwrap().method, "Target.targetCreated");
    assert_eq!(all.recv().await.unwrap().method, "Target.targetCreated");
}

#[tokio::test]
async fn attach_routes_by_session_id() {
    let (conn, mut browser) = connect();
    let c2 = conn.clone();
    let attach = tokio::spawn(async move { c2.attach("T1").await });
    let sent = browser.next_sent().await;
    assert_eq!(sent["method"], "Target.attachToTarget");
    assert_eq!(sent["params"], json!({"targetId": "T1", "flatten": true}));
    assert!(sent.get("sessionId").is_none());
    browser.respond(1, json!({"sessionId": "S1"}));
    let session = attach.await.unwrap().unwrap();
    assert_eq!(session.session_id(), Some("S1"));

    // Commands on the session carry sessionId.
    let s2 = session.clone();
    let call = tokio::spawn(async move { s2.call_raw("Page.enable", json!({})).await });
    let sent = browser.next_sent().await;
    assert_eq!(sent["sessionId"], "S1");
    assert_eq!(sent["method"], "Page.enable");
    browser.respond(2, json!({}));
    call.await.unwrap().unwrap();

    // Events with sessionId go to that session only.
    let mut on_session = session.events("Page.loadEventFired");
    let mut on_root = conn.root().events("Page.loadEventFired");
    browser.event("Page.loadEventFired", json!({"timestamp": 1.0}), Some("S1"));
    let ev = on_session.recv().await.unwrap();
    assert_eq!(ev.session_id.as_deref(), Some("S1"));
    assert_eq!(ev.params["timestamp"], 1.0);
    assert!(
        tokio::time::timeout(Duration::from_millis(30), on_root.recv())
            .await
            .is_err(),
        "root must not see a session event"
    );

    // detach() goes through the root with the sessionId and closes channels.
    let s3 = session.clone();
    let detach = tokio::spawn(async move { s3.detach().await });
    let sent = browser.next_sent().await;
    assert_eq!(sent["method"], "Target.detachFromTarget");
    assert_eq!(sent["params"], json!({"sessionId": "S1"}));
    assert!(sent.get("sessionId").is_none());
    browser.respond(3, json!({}));
    detach.await.unwrap().unwrap();
    assert!(matches!(
        on_session.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Closed)
    ));
}

#[tokio::test]
async fn detached_from_target_event_closes_session_channels() {
    let (conn, browser) = connect();
    let session = conn.session("S9");
    let mut rx = session.events("Page.frameNavigated");
    browser.event(
        "Target.detachedFromTarget",
        json!({"sessionId": "S9", "targetId": "T9"}),
        None,
    );
    assert!(matches!(
        rx.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Closed)
    ));
}

#[tokio::test]
async fn domain_guard_refcounts_enable_and_disable() {
    let (conn, mut browser) = connect();
    let root = conn.root();

    let r = root.clone();
    let g1 = tokio::spawn(async move { r.enable_domain("Page").await });
    let sent = browser.next_sent().await;
    assert_eq!(sent["method"], "Page.enable");
    browser.respond(1, json!({}));
    let g1 = g1.await.unwrap().unwrap();
    assert_eq!(root.domain_refcount("Page"), 1);

    // Second guard: no second enable on the wire.
    let g2 = root.enable_domain("Page").await.unwrap();
    assert!(browser.next_sent_within(30).await.is_none());
    assert_eq!(root.domain_refcount("Page"), 2);

    drop(g1);
    assert!(
        browser.next_sent_within(30).await.is_none(),
        "still one guard alive"
    );
    assert_eq!(root.domain_refcount("Page"), 1);

    drop(g2);
    let sent = browser
        .next_sent_within(500)
        .await
        .expect("disable after last guard");
    assert_eq!(sent["method"], "Page.disable");
    assert_eq!(root.domain_refcount("Page"), 0);
    browser.respond(sent["id"].as_u64().unwrap(), json!({}));

    // Enabling again starts a fresh cycle.
    let r = root.clone();
    let g3 = tokio::spawn(async move { r.enable_domain("Page").await });
    let sent = browser.next_sent().await;
    assert_eq!(sent["method"], "Page.enable");
    browser.respond(sent["id"].as_u64().unwrap(), json!({}));
    let _g3 = g3.await.unwrap().unwrap();

    // Guards are per session: a different session enables separately.
    let session = conn.session("S1");
    let s = session.clone();
    let g4 = tokio::spawn(async move { s.enable_domain("Page").await });
    let sent = browser.next_sent().await;
    assert_eq!(sent["method"], "Page.enable");
    assert_eq!(sent["sessionId"], "S1");
    browser.respond(sent["id"].as_u64().unwrap(), json!({}));
    let _g4 = g4.await.unwrap().unwrap();

    // Runtime / DOM are refused outright.
    assert!(root.enable_domain("Runtime").await.is_err());
    assert!(root.enable_domain("DOM").await.is_err());
    assert!(browser.next_sent_within(30).await.is_none());
}

#[tokio::test]
async fn domain_guard_failed_enable_leaves_count_at_zero() {
    let (conn, mut browser) = connect();
    let root = conn.root();
    let r = root.clone();
    let g = tokio::spawn(async move { r.enable_domain("Fetch").await });
    let sent = browser.next_sent().await;
    browser.fail(
        sent["id"].as_u64().unwrap(),
        -32601,
        "'Fetch.enable' wasn't found",
    );
    assert!(g.await.unwrap().is_err());
    assert_eq!(root.domain_refcount("Fetch"), 0);
}

#[tokio::test]
async fn forbidden_methods_are_refused_without_touching_the_wire() {
    let (conn, mut browser) = connect();
    let root = conn.root();
    for m in crate::FORBIDDEN_METHODS {
        match root.call_raw(m, json!({})).await {
            Err(CdpError::Protocol { method, .. }) => assert_eq!(&method, m),
            other => panic!("{m}: expected refusal, got {other:?}"),
        }
    }
    assert!(browser.next_sent_within(30).await.is_none());
}

#[tokio::test]
async fn hang_up_fails_pending_calls_and_closes_everything() {
    let (conn, mut browser) = connect();
    let root = conn.root();
    let mut rx = root.events("Page.loadEventFired");
    let r = root.clone();
    let call = tokio::spawn(async move { r.call_raw("Browser.getVersion", json!({})).await });
    browser.next_sent().await;
    browser.hang_up();
    assert!(matches!(call.await.unwrap(), Err(CdpError::Closed)));
    conn.wait_closed().await;
    assert!(conn.is_closed());
    assert!(matches!(
        root.call_raw("Browser.getVersion", json!({})).await,
        Err(CdpError::Closed)
    ));
    assert!(matches!(
        rx.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Closed)
    ));
    // Subscribing after close yields an already-closed receiver.
    assert!(matches!(
        root.events("X.y").recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Closed)
    ));
}

#[tokio::test]
async fn mark_crashed_fails_pending_and_future_calls_with_browser_crashed() {
    let (conn, mut browser) = connect();
    let root = conn.root();
    let r = root.clone();
    let call = tokio::spawn(async move { r.call_raw("Browser.getVersion", json!({})).await });
    browser.next_sent().await;
    conn.mark_crashed(Some(133), "[fatal] boom".into());
    match call.await.unwrap() {
        Err(CdpError::BrowserCrashed {
            exit_code,
            stderr_tail,
        }) => {
            assert_eq!(exit_code, Some(133));
            assert_eq!(stderr_tail, "[fatal] boom");
        }
        other => panic!("expected BrowserCrashed, got {other:?}"),
    }
    conn.wait_closed().await;
    assert!(conn.is_closed());
    assert!(matches!(
        root.call_raw("Browser.getVersion", json!({})).await,
        Err(CdpError::BrowserCrashed { .. })
    ));
    assert!(browser.sent.recv().await.is_none(), "transport dropped");
}

#[tokio::test]
async fn close_ends_the_driver() {
    let (conn, mut browser) = connect();
    conn.close();
    conn.wait_closed().await;
    assert!(conn.is_closed());
    assert!(browser.sent.recv().await.is_none(), "transport dropped");
}

#[tokio::test]
async fn dropping_the_last_handle_ends_the_driver() {
    let (conn, mut browser) = connect();
    drop(conn);
    assert!(browser.sent.recv().await.is_none(), "transport dropped");
}

#[tokio::test]
async fn timeout_is_reported_with_the_method() {
    let (conn, mut browser) = connect();
    let root = conn.root().with_timeout(Some(Duration::from_millis(20)));
    let call = tokio::spawn(async move { root.call_raw("Page.navigate", json!({})).await });
    browser.next_sent().await;
    match call.await.unwrap() {
        Err(CdpError::Timeout { method }) => assert_eq!(method, "Page.navigate"),
        other => panic!("expected Timeout, got {other:?}"),
    }
    // A late answer for the abandoned id is ignored, not a crash.
    browser.respond(1, json!({}));
    let root = conn.root();
    let call = tokio::spawn(async move { root.call_raw("Browser.getVersion", json!({})).await });
    let sent = browser.next_sent().await;
    browser.respond(sent["id"].as_u64().unwrap(), json!({"ok": true}));
    assert_eq!(call.await.unwrap().unwrap(), json!({"ok": true}));
}

#[tokio::test]
async fn malformed_frames_are_dropped_not_fatal() {
    let (conn, mut browser) = connect();
    browser.push_raw(b"not json");
    browser.push_raw(&[0xff, 0xfe]);
    browser.push(json!({"neither": "id nor method"}));
    browser.push(json!({"method": "X.y", "params": "not an object"}));
    let root = conn.root();
    let call = tokio::spawn(async move { root.call_raw("Browser.getVersion", json!({})).await });
    let sent = browser.next_sent().await;
    browser.respond(sent["id"].as_u64().unwrap(), json!({"ok": true}));
    assert_eq!(call.await.unwrap().unwrap(), json!({"ok": true}));
    assert!(!conn.is_closed());
}

#[tokio::test]
async fn typed_send_uses_generated_shapes() {
    use crate::protocol::target;
    let (conn, mut browser) = connect();
    let root = conn.root();
    let call = tokio::spawn(async move {
        root.send(target::CreateTarget {
            url: "about:blank".into(),
            ..Default::default()
        })
        .await
    });
    let sent = browser.next_sent().await;
    assert_eq!(sent["method"], "Target.createTarget");
    assert_eq!(sent["params"], json!({"url": "about:blank"}));
    browser.respond(1, json!({"targetId": "T1"}));
    assert_eq!(call.await.unwrap().unwrap().target_id, "T1");
}

mod trace {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;
    use tracing_subscriber::fmt::MakeWriter;

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    #[tokio::test]
    async fn trace_logs_both_directions_with_arrows() {
        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let (conn, mut browser) = connect();
        conn.set_trace(true);
        assert!(conn.trace_enabled());
        let root = conn.root();
        let call =
            tokio::spawn(async move { root.call_raw("Browser.getVersion", json!({})).await });
        browser.next_sent().await;
        // A big response exercises truncation.
        browser.respond(1, json!({"big": "x".repeat(10_000)}));
        call.await.unwrap().unwrap();
        browser.event("Page.loadEventFired", json!({"timestamp": 1}), None);
        tokio::task::yield_now().await;

        let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(log.contains("surf_cdp::trace"), "{log}");
        assert!(
            log.contains("→ {\"id\":1,\"method\":\"Browser.getVersion\""),
            "{log}"
        );
        assert!(log.contains("← {\"id\":1,\"result\""), "{log}");
        assert!(log.contains("… (+"), "long frame truncated: {log}");
        assert!(
            log.contains("← {\"method\":\"Page.loadEventFired\""),
            "{log}"
        );

        conn.set_trace(false);
        assert!(!conn.trace_enabled());
    }
}
