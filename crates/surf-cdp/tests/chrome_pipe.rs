//! End-to-end: the pipe transport + connection against a real Chrome.
//!
//! Skips (with a message) when no Chrome is found; `SURF_CHROME` overrides
//! discovery. Launch flags here are the quiet set only: no
//! `--enable-automation`, no `--disable-*`.
//!
//! Needs `unsafe` for `pre_exec` + `dup2` — the launcher proper lives in
//! `surf-browser`; this test only proves the transport contract.

#![cfg(unix)]

use serde_json::json;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use surf_cdp::protocol::{page, runtime, target};
use surf_cdp::transport::pipe;
use surf_cdp::{CdpError, Connection, ProtocolEvent};

fn find_chrome() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SURF_CHROME") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let candidates = [
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        "/Applications/Chromium.app/Contents/MacOS/Chromium",
        "/usr/bin/google-chrome",
        "/usr/bin/google-chrome-stable",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
    ];
    if let Some(p) = candidates.iter().map(PathBuf::from).find(|p| p.is_file()) {
        return Some(p);
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
        ] {
            let p = dir.join(name);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

#[tokio::test]
async fn pipe_transport_drives_real_chrome() {
    let Some(chrome) = find_chrome() else {
        eprintln!("skipping chrome_pipe: no Chrome found (set SURF_CHROME)");
        return;
    };
    let profile = tempfile::tempdir().unwrap();
    let (transport, child_fds) = pipe::create_pair().unwrap();
    let (r, w) = child_fds.raw();
    assert!(r > 4 && w > 4, "test assumes the child ends are above fd 4");

    let mut cmd = tokio::process::Command::new(&chrome);
    cmd.arg("--remote-debugging-pipe")
        .arg(format!("--user-data-dir={}", profile.path().display()))
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--headless")
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // SAFETY: only async-signal-safe calls (dup2) between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if libc::dup2(r, 3) < 0 || libc::dup2(w, 4) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().expect("spawn chrome");
    // The parent must not keep the child's ends open.
    drop(child_fds);

    let conn = Connection::new(Box::new(transport));
    let run = tokio::time::timeout(Duration::from_secs(60), exercise(&conn));
    let outcome = run.await;
    // Always tear down, then assert.
    let _ = conn.root().call_raw("Browser.close", json!({})).await;
    let exited = tokio::time::timeout(Duration::from_secs(10), child.wait()).await;
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("CDP failure: {e}"),
        Err(_) => panic!("timed out talking to Chrome"),
    }
    exited
        .expect("chrome did not exit after Browser.close")
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), conn.wait_closed())
        .await
        .expect("connection did not close after Chrome exited");
    assert!(conn.is_closed());
}

async fn exercise(conn: &std::sync::Arc<Connection>) -> Result<(), CdpError> {
    let root = conn.root().with_timeout(Some(Duration::from_secs(20)));

    // 1. root call over the pipe
    let version: serde_json::Value = root.call_raw("Browser.getVersion", json!({})).await?;
    let product = version["product"].as_str().unwrap_or_default().to_owned();
    assert!(
        product.contains("Chrome") || product.contains("Chromium"),
        "{version}"
    );
    eprintln!("chrome_pipe: {product}");

    // 2. typed command + attach + session routing
    let created = root
        .send(target::CreateTarget {
            url: "about:blank".into(),
            ..Default::default()
        })
        .await?;
    let session = conn
        .attach(&created.target_id)
        .await?
        .with_timeout(Some(Duration::from_secs(20)));
    assert!(session.session_id().is_some());

    // 3. ref-counted Page.enable, lifecycle event on the session
    let _page = session.enable_domain("Page").await?;
    assert_eq!(session.domain_refcount("Page"), 1);
    let mut loaded = session.events(page::LoadEventFiredEvent::METHOD);
    let nav = session
        .send(page::Navigate {
            url: "data:text/html,<h1 id=t>surf</h1>".into(),
            ..Default::default()
        })
        .await?;
    assert!(nav.error_text.is_none(), "{nav:?}");
    let ev = tokio::time::timeout(Duration::from_secs(20), surf_cdp::event::next(&mut loaded))
        .await
        .expect("loadEventFired")
        .expect("channel open");
    assert_eq!(ev.session_id.as_deref(), session.session_id());
    let _: page::LoadEventFiredEvent = ev.parse().unwrap();

    // 4. isolated world, no Runtime.enable
    let world = session
        .send(page::CreateIsolatedWorld {
            frame_id: nav.frame_id.clone(),
            world_name: Some("surf_test".into()),
            grant_univeral_access: Some(true),
            ..Default::default()
        })
        .await?;
    let ctx = world.execution_context_id;
    let eval = session
        .send(runtime::Evaluate {
            expression: "document.getElementById('t').textContent".into(),
            context_id: Some(ctx),
            return_by_value: Some(true),
            ..Default::default()
        })
        .await?;
    assert_eq!(eval.result.value, Some(json!("surf")), "{eval:?}");

    // 5. Runtime.addBinding + bindingCalled fire without Runtime.enable
    let mut called = session.events(runtime::BindingCalledEvent::METHOD);
    session
        .send(runtime::AddBinding {
            name: "surf_test_binding".into(),
            execution_context_id: Some(ctx),
            execution_context_name: None,
        })
        .await?;
    session
        .send(runtime::Evaluate {
            expression: "surf_test_binding('ping'); 1".into(),
            context_id: Some(ctx),
            ..Default::default()
        })
        .await?;
    let ev = tokio::time::timeout(Duration::from_secs(20), surf_cdp::event::next(&mut called))
        .await
        .expect("bindingCalled")
        .expect("channel open");
    let b: runtime::BindingCalledEvent = ev.parse().unwrap();
    assert_eq!(b.name, "surf_test_binding");
    assert_eq!(b.payload, "ping");
    assert_eq!(b.execution_context_id, ctx);

    // 6. detach closes the session's channels
    session.detach().await?;
    assert!(matches!(
        loaded.recv().await,
        Err(tokio::sync::broadcast::error::RecvError::Closed)
    ));
    Ok(())
}
