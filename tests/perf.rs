//! Performance gates, measured on the built `surf` binary:
//!
//! ```text
//! cargo test --release --test perf
//! ```
//!
//! The budgets are for a release build; in a debug build every gate prints
//! its measurement and the reason it is not asserted, then passes. Numbers
//! measured on the reference Mac are recorded in `docs/quiet-cdp.md`.
//!
//! | gate | budget |
//! |------|--------|
//! | `surf run tests/vm/noop.surf` process wall time, median of 20 | < 10 ms |
//! | `surf` RSS while idle with one browser attached (`ps -o rss=`) | < 15 MB |
//! | `surf run` start → first CDP frame sent (from `--trace-cdp`) | reported; < 500 ms |
//!
//! Gates that need Chrome skip with a message when none is found
//! (`SURF_CHROME` overrides discovery).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use surf_browser::discovery::chrome_or_skip;

/// Cold-start budget for the no-op script (release).
const NOOP_BUDGET: Duration = Duration::from_millis(10);
/// Idle RSS budget with one browser attached (release).
const RSS_BUDGET_KB: u64 = 15 * 1024;
/// Start → first CDP frame budget (release; includes spawning Chrome).
const FIRST_FRAME_BUDGET: Duration = Duration::from_millis(500);

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn release() -> bool {
    !cfg!(debug_assertions)
}

fn gate(name: &str, measured: &str, ok: bool) {
    if release() {
        eprintln!("perf {name}: {measured}");
        assert!(ok, "perf gate {name} failed: {measured}");
    } else {
        eprintln!(
            "perf {name}: {measured} — debug build, budget not asserted (run with --release)"
        );
    }
}

/// `surf run tests/vm/noop.surf` (parse + compile + run, no browser) —
/// process wall time including exec, median of 20 after 3 warm-ups.
#[test]
fn noop_cold_start_median_under_budget() {
    let script = workspace_root().join("tests/vm/noop.surf");
    assert!(script.is_file(), "{}", script.display());
    let run = || {
        let t = Instant::now();
        let out = Command::new(env!("CARGO_BIN_EXE_surf"))
            .arg("run")
            .arg(&script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("run surf");
        assert!(out.success());
        t.elapsed()
    };
    for _ in 0..3 {
        run();
    }
    let mut samples: Vec<Duration> = (0..20).map(|_| run()).collect();
    samples.sort();
    let median = samples[samples.len() / 2];
    let min = samples[0];
    let max = samples[samples.len() - 1];
    gate(
        "noop cold start",
        &format!(
            "median {:.2} ms (min {:.2}, max {:.2}) over 20 runs; budget {} ms",
            median.as_secs_f64() * 1e3,
            min.as_secs_f64() * 1e3,
            max.as_secs_f64() * 1e3,
            NOOP_BUDGET.as_millis()
        ),
        median < NOOP_BUDGET,
    );
}

/// Direct children of `pid` whose command line carries the pipe flag —
/// the browser process (not the transient `chrome --version` probe).
fn child_pids(pid: u32) -> Vec<u32> {
    let Ok(out) = Command::new("pgrep")
        .args(["-P", &pid.to_string(), "-f", "remote-debugging-pipe"])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect()
}

/// Resident set size of `pid` in KiB (`ps -o rss=`).
fn rss_kb(pid: u32) -> Option<u64> {
    let out = Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// `surf` holding one launched browser and sleeping: its own RSS (the
/// browser's is not counted) stays under the budget.
#[test]
fn idle_rss_with_one_browser_under_budget() {
    let Some(chrome) = chrome_or_skip("idle_rss_with_one_browser_under_budget") else {
        return;
    };
    if !cfg!(unix) {
        eprintln!("skipping idle_rss_with_one_browser_under_budget: ps/pgrep gate is unix-only");
        return;
    }
    let tmp = tempfile::tempdir().expect("tempdir");
    let script = tmp.path().join("idle.surf");
    std::fs::write(&script, "goto(\"about:blank\")\nsleep(4s)\n").unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_surf"))
        .arg("run")
        .arg(&script)
        .arg("--headless")
        .env("SURF_CHROME", &chrome)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn surf");
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut browser = child_pids(pid);
    while Instant::now() < deadline && browser.is_empty() {
        std::thread::sleep(Duration::from_millis(50));
        browser = child_pids(pid);
    }
    assert!(!browser.is_empty(), "surf never started a browser");
    // Let the launch handshake and the page creation settle, then sample
    // while the script sleeps.
    std::thread::sleep(Duration::from_millis(1500));
    let rss = rss_kb(pid);
    let out = child.wait_with_output().expect("surf exit");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rss = rss.expect("ps -o rss=");
    gate(
        "idle RSS with one browser",
        &format!(
            "{:.1} MB ({rss} KiB); budget {} MB",
            rss as f64 / 1024.0,
            RSS_BUDGET_KB / 1024
        ),
        rss < RSS_BUDGET_KB,
    );
}

/// `--trace-cdp` reports the time from process start to the first CDP
/// frame (the `Browser.getVersion` handshake right after spawning Chrome);
/// it exists and is under the budget.
#[test]
fn first_cdp_frame_time_is_reported() {
    let Some(chrome) = chrome_or_skip("first_cdp_frame_time_is_reported") else {
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let script = tmp.path().join("first.surf");
    std::fs::write(&script, "goto(\"about:blank\")\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_surf"))
        .arg("run")
        .arg(&script)
        .arg("--headless")
        .arg("--trace-cdp")
        .env("SURF_CHROME", &chrome)
        .env_remove("RUST_LOG")
        .output()
        .expect("run surf");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    let re =
        regex::Regex::new(r"first CDP frame sent ([0-9]+(?:\.[0-9]+)?) ms after start").unwrap();
    let ms: f64 = re
        .captures(&stderr)
        .and_then(|c| c[1].parse().ok())
        .unwrap_or_else(|| panic!("no first-frame line in --trace-cdp output:\n{stderr}"));
    assert!(
        stderr.contains("→ {\"id\":1,\"method\":\"Browser.getVersion\""),
        "trace does not show the first frame:\n{stderr}"
    );
    gate(
        "start → first CDP frame",
        &format!("{ms:.1} ms; budget {} ms", FIRST_FRAME_BUDGET.as_millis()),
        ms < FIRST_FRAME_BUDGET.as_secs_f64() * 1e3,
    );
}
