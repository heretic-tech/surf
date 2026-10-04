//! End-to-end tests through the built `surf` binary against a real Chrome
//! and the in-process fixture server (`surf-testserver`).
//!
//! `tests/e2e/scripts/<name>.surf` is run with `surf run --headless`; its
//! stdout must equal `<name>.out` byte for byte and the exit code must be
//! `0` (or the number in `<name>.code` when present). Scripts reach the
//! fixture server through `env("SURF_E2E_BASE")` and a scratch directory
//! through `env("SURF_E2E_TMP")`.
//!
//! Skip (with a printed message) when no Chrome is found; `SURF_CHROME`
//! overrides discovery. These MUST run on developer Macs and in the `e2e`
//! CI job.

use std::path::{Path, PathBuf};
use std::process::Command;
use surf_browser::discovery::chrome_or_skip;
use surf_testserver::Fixture;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scripts_dir() -> PathBuf {
    workspace_root().join("tests/e2e/scripts")
}

/// A fixture server on its own multi-thread runtime, so the child `surf`
/// processes can talk to it while the test thread blocks on them.
struct Server {
    _rt: tokio::runtime::Runtime,
    fixture: Fixture,
}

impl Server {
    fn start() -> Server {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let fixture = rt.block_on(Fixture::start());
        Server { _rt: rt, fixture }
    }
}

struct Outcome {
    stdout: String,
    stderr: String,
    code: i32,
}

fn run_surf(chrome: &Path, server: &Server, tmp: &Path, script: &Path, extra: &[&str]) -> Outcome {
    let out = Command::new(env!("CARGO_BIN_EXE_surf"))
        .arg("run")
        .arg(script)
        .arg("--headless")
        .arg("--timeout")
        .arg("15s")
        .args(extra)
        .env("SURF_CHROME", chrome)
        .env("SURF_E2E_BASE", &server.fixture.base)
        .env("SURF_E2E_TMP", tmp)
        .env("NO_COLOR", "1")
        .env_remove("SURF_TRACE_CDP")
        .output()
        .expect("run surf");
    Outcome {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

#[test]
fn chrome_is_discoverable() {
    let Some(path) = chrome_or_skip("chrome_is_discoverable") else {
        return;
    };
    assert!(path.is_file(), "{} is not a file", path.display());
}

#[test]
fn scripts_match_expected_output() {
    let Some(chrome) = chrome_or_skip("scripts_match_expected_output") else {
        return;
    };
    let server = Server::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut scripts: Vec<PathBuf> = std::fs::read_dir(scripts_dir())
        .expect("tests/e2e/scripts")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "surf"))
        .collect();
    scripts.sort();
    assert!(!scripts.is_empty(), "no e2e scripts found");
    let mut failures = Vec::new();
    for script in &scripts {
        let name = script.file_stem().unwrap().to_string_lossy().into_owned();
        let expected = std::fs::read_to_string(script.with_extension("out"))
            .unwrap_or_else(|_| panic!("missing {name}.out"));
        let expected_code: i32 = std::fs::read_to_string(script.with_extension("code"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let started = std::time::Instant::now();
        let r = run_surf(&chrome, &server, tmp.path(), script, &[]);
        eprintln!(
            "e2e {name}: exit {} in {:.1}s",
            r.code,
            started.elapsed().as_secs_f64()
        );
        if r.stdout != expected || r.code != expected_code {
            failures.push(format!(
                "--- {name}: exit {} (expected {expected_code})\n--- stdout:\n{}--- expected:\n{}--- stderr:\n{}",
                r.code, r.stdout, expected, r.stderr
            ));
        }
    }
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

#[test]
fn json_mode_makes_print_a_json_line() {
    let Some(chrome) = chrome_or_skip("json_mode_makes_print_a_json_line") else {
        return;
    };
    let server = Server::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let script = tmp.path().join("json.surf");
    std::fs::write(
        &script,
        "base = env(\"SURF_E2E_BASE\")\ngoto(\"{base}/\")\nprint(\"hi\", 1)\nemit {h1: text(\"h1\"), n: 2}\n",
    )
    .unwrap();
    let r = run_surf(&chrome, &server, tmp.path(), &script, &["--json"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(
        r.stdout,
        "{\"print\":\"hi 1\"}\n{\"h1\":\"Index\",\"n\":2}\n"
    );
}

#[test]
fn runtime_error_renders_selector_and_exits_1() {
    let Some(chrome) = chrome_or_skip("runtime_error_renders_selector_and_exits_1") else {
        return;
    };
    let server = Server::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let script = tmp.path().join("err.surf");
    std::fs::write(
        &script,
        "base = env(\"SURF_E2E_BASE\")\ngoto(\"{base}/\")\nclick(\"#missing\", timeout: 300ms)\nprint(\"unreachable\")\n",
    )
    .unwrap();
    let r = run_surf(&chrome, &server, tmp.path(), &script, &[]);
    assert_eq!(r.code, 1, "{}", r.stderr);
    assert_eq!(r.stdout, "");
    assert!(r.stderr.contains("timed out after 0.3s"), "{}", r.stderr);
    assert!(r.stderr.contains("selector: #missing"), "{}", r.stderr);
    assert!(r.stderr.contains("err.surf:3:1"), "{}", r.stderr);
}

#[test]
fn exit_code_and_shebang_shorthand() {
    let Some(chrome) = chrome_or_skip("exit_code_and_shebang_shorthand") else {
        return;
    };
    let tmp = tempfile::tempdir().expect("tempdir");
    let script = tmp.path().join("exit.surf");
    std::fs::write(
        &script,
        "#!/usr/bin/env surf\nprint(\"before\")\nexit(7)\nprint(\"after\")\n",
    )
    .unwrap();
    // `surf <file>` shorthand (no `run`); no browser is launched since no
    // action runs, so this is fast even with Chrome around.
    let out = Command::new(env!("CARGO_BIN_EXE_surf"))
        .arg(&script)
        .env("SURF_CHROME", &chrome)
        .env("NO_COLOR", "1")
        .output()
        .expect("run surf");
    assert_eq!(out.status.code(), Some(7));
    assert_eq!(String::from_utf8_lossy(&out.stdout), "before\n");
}

#[test]
fn check_reports_syntax_errors_with_exit_2() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let script = tmp.path().join("bad.surf");
    std::fs::write(&script, "if x\n    print(1)\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_surf"))
        .arg("check")
        .arg(&script)
        .env("NO_COLOR", "1")
        .output()
        .expect("run surf");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("error"));
    let ok = tmp.path().join("ok.surf");
    std::fs::write(&ok, "x = 1\nprint(x)\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_surf"))
        .arg("check")
        .arg(&ok)
        .output()
        .expect("run surf");
    assert_eq!(out.status.code(), Some(0));
}

#[test]
fn doctor_launches_over_the_pipe() {
    let Some(chrome) = chrome_or_skip("doctor_launches_over_the_pipe") else {
        return;
    };
    let out = Command::new(env!("CARGO_BIN_EXE_surf"))
        .arg("doctor")
        .env("SURF_CHROME", &chrome)
        .output()
        .expect("run surf");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("launch:   ok"), "{stdout}");
    let flags = stdout
        .lines()
        .find(|l| l.starts_with("flags:"))
        .unwrap_or_else(|| panic!("no flags line in {stdout}"));
    assert!(flags.contains("--remote-debugging-pipe"), "{flags}");
    assert!(flags.contains("--user-data-dir="), "{flags}");
    assert!(!flags.contains("--enable-automation"), "{flags}");
}

#[test]
fn repl_keeps_the_page_between_lines() {
    let Some(chrome) = chrome_or_skip("repl_keeps_the_page_between_lines") else {
        return;
    };
    let server = Server::start();
    let input = format!(
        "goto(\"{}/forms.html\")\ntype(\"#username\", \"abc\")\nprint(value(\"#username\"))\nfn twice(x):\n    return x * 2\n\nprint(twice(21))\n.exit\n",
        server.fixture.base
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_surf"))
        .arg("repl")
        .arg("--headless")
        .env("SURF_CHROME", &chrome)
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn surf repl");
    {
        use std::io::Write;
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(input.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().expect("repl output");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(stdout, "abc\n42\n", "{stderr}");
}

#[test]
fn examples_are_present() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    for name in [
        "hello.surf",
        "login.surf",
        "two-tabs.surf",
        "scrape-emit.surf",
        "parallel-pool.surf",
        "supervised.surf",
    ] {
        let p = std::path::Path::new(dir).join(name);
        assert!(p.is_file(), "missing example {}", p.display());
    }
}
