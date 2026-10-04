//! Launch a real Chrome through `surf_browser::launch` and check the quiet
//! contract: pipe transport (no listening sockets), temp profile removed on
//! close, `navigator.webdriver === false`, no automation infobar, and the
//! one permitted `--disable-*` switch (DECISIONS.md #12) — with a test
//! showing what happens without it.
//!
//! Skips (printing a message) when no Chrome is found; `SURF_CHROME`
//! overrides discovery. These MUST run on developer machines and in CI.
//! Nothing in here (or in `surf-browser`) ever sends `Runtime.enable`.

use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;
use surf_browser::discovery::chrome_or_skip;
use surf_browser::launch::{launch, LaunchConfig, Launched, TransportChoice};
use surf_cdp::protocol::{page, runtime, target};

/// Headless launch over the pipe: `Browser.getVersion`, create + attach a
/// target, no TCP listeners owned by the browser pid, temp profile gone
/// after close.
#[tokio::test]
async fn headless_pipe_launch_is_quiet_and_cleans_up() {
    let Some(chrome) = chrome_or_skip("headless_pipe_launch_is_quiet_and_cleans_up") else {
        return;
    };
    let cfg = LaunchConfig::for_path(chrome);
    let launched = launch(cfg).await.expect("launch");
    let profile = launched.profile_dir.path().to_path_buf();
    assert!(launched.profile_dir.is_temp());
    assert!(profile.is_dir(), "temp profile should exist while running");
    assert!(
        launched.product.contains("Chrome") || launched.product.contains("Chromium"),
        "{}",
        launched.product
    );
    assert!(!launched
        .args
        .iter()
        .any(|a| a.contains("enable-automation")));
    // DECISIONS.md #12: exactly one --disable-* switch, and no other.
    let disables: Vec<_> = launched
        .args
        .iter()
        .filter(|a| a.starts_with("--disable-"))
        .collect();
    assert_eq!(
        disables,
        vec![surf_browser::launch::AUTOMATION_CONTROLLED_OFF]
    );
    eprintln!("launch: {} pid {}", launched.product, launched.pid());

    let outcome = tokio::time::timeout(Duration::from_secs(30), exercise(&launched)).await;
    let listeners = listening_sockets(launched.pid());
    launched.close().await;

    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("CDP failure: {e}"),
        Err(_) => panic!("timed out talking to Chrome"),
    }
    match listeners {
        Some(lines) => assert!(
            lines.is_empty(),
            "browser pid owns listening TCP sockets:\n{}",
            lines.join("\n")
        ),
        None => eprintln!("lsof not available; skipping listening-socket check"),
    }
    assert!(
        launched.process.has_exited(),
        "process should have exited after close"
    );
    assert!(launched.connection.is_closed());
    assert!(
        !profile.exists(),
        "temp profile {} should be removed on close",
        profile.display()
    );
}

async fn exercise(launched: &Launched) -> Result<(), surf_cdp::CdpError> {
    let root = launched.root().with_timeout(Some(Duration::from_secs(20)));
    let v = root
        .send(surf_cdp::protocol::browser::GetVersion {})
        .await?;
    assert!(!v.product.is_empty());
    assert!(!v.user_agent.is_empty());

    let created = root
        .send(target::CreateTarget {
            url: "about:blank".into(),
            ..Default::default()
        })
        .await?;
    let session = launched
        .connection
        .attach(&created.target_id)
        .await?
        .with_timeout(Some(Duration::from_secs(20)));
    assert!(session.session_id().is_some());
    let tree = session.send(page::GetFrameTree {}).await?;
    assert!(!tree.frame_tree.frame.id.is_empty());
    session.detach().await?;
    root.send(target::CloseTarget {
        target_id: created.target_id,
    })
    .await?;
    Ok(())
}

/// Headed launch at 1280×800, evaluated in a fresh isolated world
/// (`Page.createIsolatedWorld` → `Runtime.evaluate{contextId}`, never
/// `Runtime.enable`): `navigator.webdriver === false` and the window chrome
/// (outer − inner height) is below 120 px, i.e. no "controlled by
/// automated software" infobar.
#[tokio::test]
async fn headed_launch_has_no_webdriver_flag_and_no_infobar() {
    let Some(chrome) = chrome_or_skip("headed_launch_has_no_webdriver_flag_and_no_infobar") else {
        return;
    };
    if !surf_browser::display::has_display() {
        eprintln!("skipping headed_launch_has_no_webdriver_flag_and_no_infobar: no display");
        return;
    }
    let cfg = LaunchConfig {
        headless: false,
        size: (1280, 800),
        ..LaunchConfig::for_path(chrome)
    };
    let launched = launch(cfg).await.expect("launch");
    let outcome = tokio::time::timeout(Duration::from_secs(30), probe_headed(&launched)).await;
    launched.close().await;
    match outcome {
        Ok(Ok((webdriver, chrome_px, inner))) => {
            eprintln!(
                "headed: navigator.webdriver={webdriver} outer-inner={chrome_px}px inner={inner}"
            );
            assert_eq!(webdriver, json!(false), "navigator.webdriver must be false");
            assert!(
                chrome_px < 120,
                "outerHeight - innerHeight = {chrome_px}px: looks like an infobar"
            );
        }
        Ok(Err(e)) => panic!("CDP failure: {e}"),
        Err(_) => panic!("timed out talking to Chrome"),
    }
}

/// Why `--disable-blink-features=AutomationControlled` is in the flag set
/// (DECISIONS.md #12): without it, stock Chrome reports
/// `navigator.webdriver === true` merely because `--remote-debugging-pipe`
/// is on the command line — no `--enable-automation`, no session attached
/// to the page. Measured on Chrome 154 stable and Chrome for Testing
/// 133 / 152 (https://issues.chromium.org/issues/40746300). This test
/// exists so a future "cleanup" cannot remove the switch unnoticed.
#[tokio::test]
async fn webdriver_is_true_without_automationcontrolled_switch() {
    let Some(chrome) = chrome_or_skip("webdriver_is_true_without_automationcontrolled_switch")
    else {
        return;
    };
    let cfg = LaunchConfig {
        correct_automation_controlled: false,
        ..LaunchConfig::for_path(chrome)
    };
    let launched = launch(cfg).await.expect("launch");
    assert!(!launched.args.iter().any(|a| a.starts_with("--disable-")));
    let outcome = tokio::time::timeout(Duration::from_secs(30), probe_headed(&launched)).await;
    launched.close().await;
    match outcome {
        Ok(Ok((webdriver, _, _))) => {
            eprintln!("without switch: navigator.webdriver={webdriver}");
            assert_eq!(
                webdriver,
                json!(true),
                "Chrome no longer sets navigator.webdriver for a debugger pipe — revisit DECISIONS.md #12"
            );
        }
        Ok(Err(e)) => panic!("CDP failure: {e}"),
        Err(_) => panic!("timed out talking to Chrome"),
    }
}

async fn probe_headed(
    launched: &Launched,
) -> Result<(serde_json::Value, i64, i64), surf_cdp::CdpError> {
    let root = launched.root().with_timeout(Some(Duration::from_secs(20)));
    let created = root
        .send(target::CreateTarget {
            url: "about:blank".into(),
            ..Default::default()
        })
        .await?;
    let session = launched
        .connection
        .attach(&created.target_id)
        .await?
        .with_timeout(Some(Duration::from_secs(20)));
    let tree = session.send(page::GetFrameTree {}).await?;
    let world = session
        .send(page::CreateIsolatedWorld {
            frame_id: tree.frame_tree.frame.id.clone(),
            world_name: Some("surf_launch_test".into()),
            grant_univeral_access: Some(true),
            ..Default::default()
        })
        .await?;
    let eval = session
        .send(runtime::Evaluate {
            expression: "({ webdriver: navigator.webdriver, chrome: window.outerHeight - window.innerHeight, inner: window.innerHeight })".into(),
            context_id: Some(world.execution_context_id),
            return_by_value: Some(true),
            ..Default::default()
        })
        .await?;
    let v = eval.result.value.unwrap_or(json!(null));
    Ok((
        v["webdriver"].clone(),
        v["chrome"].as_i64().unwrap_or(i64::MAX),
        v["inner"].as_i64().unwrap_or(0),
    ))
}

/// `cdp: 0` (port mode): Chrome picks a port, Surf reads
/// `DevToolsActivePort`, connects over websocket, and the browser pid now
/// does own exactly one loopback listener (the user asked for it).
#[tokio::test]
async fn port_mode_connects_via_devtools_active_port() {
    let Some(chrome) = chrome_or_skip("port_mode_connects_via_devtools_active_port") else {
        return;
    };
    let cfg = LaunchConfig {
        transport: TransportChoice::Port(0),
        ..LaunchConfig::for_path(chrome)
    };
    let launched = launch(cfg).await.expect("launch");
    assert_eq!(launched.args[0], "--remote-debugging-port=0");
    let outcome = tokio::time::timeout(Duration::from_secs(30), exercise(&launched)).await;
    let listeners = listening_sockets(launched.pid());
    launched.close().await;
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(e)) => panic!("CDP failure: {e}"),
        Err(_) => panic!("timed out talking to Chrome"),
    }
    if let Some(lines) = listeners {
        assert!(
            lines
                .iter()
                .all(|l| l.contains("127.0.0.1:") || l.contains("localhost:")),
            "port mode must listen on loopback only:\n{}",
            lines.join("\n")
        );
        assert!(
            !lines.is_empty(),
            "expected the DevTools listener in port mode"
        );
    }
}

/// A bogus binary fails fast with a clear error (and no leftovers).
#[tokio::test]
async fn bogus_binary_fails_with_launch_error() {
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("not-chrome");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho 'fake chrome: refusing' >&2\nexit 7\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let err = launch(LaunchConfig::for_path(PathBuf::from(&fake)))
        .await
        .expect_err("fake chrome must not launch");
    let msg = err.to_string();
    eprintln!("bogus launch error: {msg}");
    assert!(
        msg.contains("exited during startup") || msg.contains("did not answer"),
        "{msg}"
    );
    assert!(
        msg.contains("fake chrome: refusing"),
        "stderr tail missing: {msg}"
    );
}

/// `lsof -iTCP -sTCP:LISTEN -a -p <pid>`: the listening sockets owned by
/// `pid` (without the header line). `None` if lsof is unavailable.
fn listening_sockets(pid: u32) -> Option<Vec<String>> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return None;
    }
    let out = std::process::Command::new("lsof")
        .args([
            "-iTCP",
            "-sTCP:LISTEN",
            "-a",
            "-p",
            &pid.to_string(),
            "-n",
            "-P",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    Some(
        text.lines()
            .filter(|l| !l.starts_with("COMMAND") && !l.trim().is_empty())
            .map(str::to_owned)
            .collect(),
    )
}
