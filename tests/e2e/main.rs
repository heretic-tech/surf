//! End-to-end tests through the built `surf` binary against a real Chrome
//! and the in-process fixture server (`surf-testserver`).
//!
//! `cargo test --test e2e` is the one command (the target is declared in
//! `crates/surf-cli/Cargo.toml` with its path pointing here, so
//! `CARGO_BIN_EXE_surf` resolves). Every `tests/e2e/scripts/<name>.surf`
//! is run with `surf run --headless` (`SURF_E2E_HEADED=1` forces headed;
//! a script named `<name>-headed.surf` always runs `--headed` and is
//! skipped when no display exists); its stdout must equal `<name>.out`
//! after normalisation and the exit
//! code must be `0` (or the number in `<name>.code`). When `<name>.err`
//! exists, stderr must match it too (error-message snapshots). A mismatch
//! prints a unified diff. `SURF_E2E_FILTER=<substring>` runs a subset;
//! `SURF_E2E_UPDATE=1` rewrites the expectation files (review the diff).
//!
//! Normalisation replaces volatile text before comparing: the scratch
//! directory (`<TMP>`), the scripts directory (`<SCRIPTS>`), the fixture
//! base URLs (`<BASE>`, `<PROXIED_BASE>`), the proxy URLs (`<PROXY_A>`,
//! `<PROXY_B>`, `<PROXY_AUTH>`), unix timestamps (`<TS>`) and elapsed
//! times such as `after 0.3s` (`after <T>`).
//!
//! Scripts reach the fixture server through `env("SURF_E2E_BASE")`, a
//! scratch directory through `env("SURF_E2E_TMP")`, two plain forward-proxy
//! stubs through `env("SURF_E2E_PROXY_A")` / `env("SURF_E2E_PROXY_B")`, an
//! authenticating one (`user:pass@` in the URL) through
//! `env("SURF_E2E_PROXY_AUTH")` (they tag what they forward, see
//! `surf_testserver::Proxy`) and the fixture server under a non-loopback
//! host name through `env("SURF_E2E_PROXIED_BASE")` (`http://surf.test:<port>`
//! — Chrome never proxies loopback hosts).
//!
//! Skip (with a printed message) when no Chrome is found; `SURF_CHROME`
//! overrides discovery. These MUST run on developer Macs and in the `e2e`
//! CI job (under `xvfb-run` on Linux).

use std::path::{Path, PathBuf};
use std::process::Command;
use surf_browser::discovery::chrome_or_skip;
use surf_testserver::{Fixture, Proxy};

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
    proxy_a: Proxy,
    proxy_b: Proxy,
    proxy_auth: Proxy,
}

impl Server {
    fn start() -> Server {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("tokio runtime");
        let fixture = rt.block_on(Fixture::start());
        let proxy_a = rt.block_on(Proxy::start("A"));
        let proxy_b = rt.block_on(Proxy::start("B"));
        let proxy_auth = rt.block_on(Proxy::start_with_auth("AUTH", "surf", "s3cret"));
        Server {
            _rt: rt,
            fixture,
            proxy_a,
            proxy_b,
            proxy_auth,
        }
    }

    /// The fixture under a host name Chrome does not treat as loopback.
    fn proxied_base(&self) -> String {
        let port = self.fixture.base.rsplit(':').next().unwrap_or("80");
        format!("http://surf.test:{port}")
    }
}

struct Outcome {
    stdout: String,
    stderr: String,
    code: i32,
}

fn headed() -> bool {
    std::env::var("SURF_E2E_HEADED").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// `<name>-headed.surf` scripts run headed (the headed detector run).
fn wants_headed(script: &Path) -> bool {
    script
        .file_stem()
        .is_some_and(|s| s.to_string_lossy().ends_with("-headed"))
}

fn run_surf(chrome: &Path, server: &Server, tmp: &Path, script: &Path, extra: &[&str]) -> Outcome {
    let out = Command::new(env!("CARGO_BIN_EXE_surf"))
        .arg("run")
        .arg(script)
        .arg(if headed() || wants_headed(script) {
            "--headed"
        } else {
            "--headless"
        })
        .arg("--timeout")
        .arg("15s")
        .args(extra)
        .env("SURF_CHROME", chrome)
        .env("SURF_E2E_BASE", &server.fixture.base)
        .env("SURF_E2E_PROXIED_BASE", server.proxied_base())
        .env("SURF_E2E_PROXY_A", &server.proxy_a.url)
        .env("SURF_E2E_PROXY_B", &server.proxy_b.url)
        .env("SURF_E2E_PROXY_AUTH", &server.proxy_auth.url)
        .env("SURF_E2E_TMP", tmp)
        .env("NO_COLOR", "1")
        .env_remove("SURF_TRACE_CDP")
        .current_dir(tmp)
        .output()
        .expect("run surf");
    Outcome {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code().unwrap_or(-1),
    }
}

/// Replace volatile text (paths, ports, timestamps, elapsed times).
fn normalise(text: &str, server: &Server, tmp: &Path) -> String {
    let scripts = scripts_dir()
        .canonicalize()
        .unwrap_or_else(|_| scripts_dir());
    let tmp_canon = tmp.canonicalize().unwrap_or_else(|_| tmp.to_path_buf());
    let mut s = text
        .replace(&tmp_canon.to_string_lossy().into_owned(), "<TMP>")
        .replace(&tmp.to_string_lossy().into_owned(), "<TMP>")
        .replace(&scripts.to_string_lossy().into_owned(), "<SCRIPTS>")
        .replace(&scripts_dir().to_string_lossy().into_owned(), "<SCRIPTS>")
        .replace(&server.proxied_base(), "<PROXIED_BASE>")
        .replace(&server.fixture.base, "<BASE>")
        .replace(&server.proxy_auth.url, "<PROXY_AUTH>")
        .replace(&server.proxy_a.url, "<PROXY_A>")
        .replace(&server.proxy_b.url, "<PROXY_B>");
    // Proxy URLs without credentials (as Chrome / the runtime report them).
    for (p, tag) in [
        (&server.proxy_a, "<PROXY_A>"),
        (&server.proxy_b, "<PROXY_B>"),
        (&server.proxy_auth, "<PROXY_AUTH>"),
    ] {
        if let Some(host) = p.url.rsplit('@').next() {
            s = s.replace(&format!("http://{host}"), tag);
        }
    }
    let ts = regex::Regex::new(r"\b1[0-9]{9}(\.[0-9]+)?\b").unwrap();
    let s = ts.replace_all(&s, "<TS>").into_owned();
    let elapsed = regex::Regex::new(r"\b(after|in|waited) [0-9]+(\.[0-9]+)?(ms|s)\b").unwrap();
    elapsed.replace_all(&s, "$1 <T>").into_owned()
}

fn unified_diff(name: &str, expected: &str, actual: &str) -> String {
    similar::TextDiff::from_lines(expected, actual)
        .unified_diff()
        .context_radius(3)
        .header(&format!("{name} (expected)"), &format!("{name} (actual)"))
        .to_string()
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
    let filter = std::env::var("SURF_E2E_FILTER").unwrap_or_default();
    let update = std::env::var("SURF_E2E_UPDATE").is_ok_and(|v| !v.is_empty() && v != "0");
    let mut scripts: Vec<PathBuf> = std::fs::read_dir(scripts_dir())
        .expect("tests/e2e/scripts")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "surf"))
        .filter(|p| {
            filter.is_empty()
                || p.file_stem()
                    .is_some_and(|s| s.to_string_lossy().contains(&filter))
        })
        .collect();
    scripts.sort();
    assert!(!scripts.is_empty(), "no e2e scripts found");
    let mut failures = Vec::new();
    for script in &scripts {
        let name = script.file_stem().unwrap().to_string_lossy().into_owned();
        if wants_headed(script) && !surf_browser::display::has_display() {
            eprintln!("e2e {name}: skipped (needs a display)");
            continue;
        }
        // Each script gets a clean scratch directory (downloads, exports).
        let scratch = tmp.path().join(&name);
        std::fs::create_dir_all(&scratch).unwrap();
        let expected_code: i32 = std::fs::read_to_string(script.with_extension("code"))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        let started = std::time::Instant::now();
        let r = run_surf(&chrome, &server, &scratch, script, &[]);
        let stdout = normalise(&r.stdout, &server, &scratch);
        let stderr = normalise(&r.stderr, &server, &scratch);
        eprintln!(
            "e2e {name}: exit {} in {:.1}s",
            r.code,
            started.elapsed().as_secs_f64()
        );
        if update {
            std::fs::write(script.with_extension("out"), &stdout).unwrap();
            if script.with_extension("err").exists() {
                std::fs::write(script.with_extension("err"), &stderr).unwrap();
            }
            continue;
        }
        let expected = std::fs::read_to_string(script.with_extension("out"))
            .unwrap_or_else(|_| panic!("missing {name}.out"));
        let expected_err = std::fs::read_to_string(script.with_extension("err")).ok();
        let mut problems = Vec::new();
        if r.code != expected_code {
            problems.push(format!("exit code {} (expected {expected_code})", r.code));
        }
        if stdout != expected {
            problems.push(format!(
                "stdout differs:\n{}",
                unified_diff(&format!("{name}.out"), &expected, &stdout)
            ));
        }
        if let Some(e) = &expected_err {
            if &stderr != e {
                problems.push(format!(
                    "stderr differs:\n{}",
                    unified_diff(&format!("{name}.err"), e, &stderr)
                ));
            }
        }
        if !problems.is_empty() {
            let mut report = format!("--- {name}: {}", problems.join("\n"));
            if expected_err.is_none() {
                report.push_str(&format!("\n--- stderr:\n{stderr}"));
            }
            failures.push(report);
        }
    }
    assert!(
        failures.is_empty(),
        "\n{} of {} scripts failed\n{}",
        failures.len(),
        scripts.len(),
        failures.join("\n")
    );
}

/// 50 concurrent pages in one browser (`parallel for` without a limit)
/// complete well under the 30 s budget.
#[test]
fn fifty_concurrent_pages_under_thirty_seconds() {
    let Some(chrome) = chrome_or_skip("fifty_concurrent_pages_under_thirty_seconds") else {
        return;
    };
    let server = Server::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let script = tmp.path().join("fifty.surf");
    std::fs::write(
        &script,
        "base = env(\"SURF_E2E_BASE\")\nparallel for i in 1..=50:\n    goto(\"{base}/slow?ms=100&i={i}\")\n    emit {i: i, slow: text(\"#slow\"), page: page.index}\n",
    )
    .unwrap();
    let started = std::time::Instant::now();
    let r = run_surf(&chrome, &server, tmp.path(), &script, &[]);
    let elapsed = started.elapsed();
    assert_eq!(r.code, 0, "{}", r.stderr);
    let lines: Vec<serde_json::Value> = r
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).expect("json line"))
        .collect();
    assert_eq!(lines.len(), 50, "{}", r.stdout);
    let mut pages: Vec<u64> = lines.iter().map(|l| l["page"].as_u64().unwrap()).collect();
    pages.sort_unstable();
    pages.dedup();
    assert_eq!(pages.len(), 50, "every item has its own page");
    assert!(lines.iter().all(|l| l["slow"] == "slept 100"));
    eprintln!(
        "50 concurrent pages: {:.1}s end to end",
        elapsed.as_secs_f64()
    );
    assert!(
        elapsed < std::time::Duration::from_secs(30),
        "took {elapsed:?}"
    );
}

/// Port-exposure gate (quiet rule 4): while a script holds a browser open
/// over the default pipe transport, neither the Chrome process nor `surf`
/// itself owns a listening TCP socket (`lsof -iTCP -sTCP:LISTEN -a -p`).
#[test]
fn pipe_transport_opens_no_listening_port() {
    let Some(chrome) = chrome_or_skip("pipe_transport_opens_no_listening_port") else {
        return;
    };
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        eprintln!("skipping pipe_transport_opens_no_listening_port: lsof gate is unix-only");
        return;
    }
    let server = Server::start();
    let tmp = tempfile::tempdir().expect("tempdir");
    let script = tmp.path().join("hold.surf");
    // Launch, load a page, then keep the browser up for a while.
    std::fs::write(
        &script,
        "base = env(\"SURF_E2E_BASE\")\ngoto(\"{base}/\")\nprint(text(\"h1\"))\nsleep(4s)\n",
    )
    .unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_surf"))
        .arg("run")
        .arg(&script)
        .arg("--headless")
        .env("SURF_CHROME", &chrome)
        .env("SURF_E2E_BASE", &server.fixture.base)
        .env("NO_COLOR", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn surf");
    let surf_pid = child.id();
    // The browser is a direct child of `surf`; wait for it to appear and
    // give it a moment to finish starting (a DevTools listener, if any,
    // is bound during startup).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut chrome_pids = Vec::new();
    while std::time::Instant::now() < deadline {
        chrome_pids = child_pids(surf_pid);
        if !chrome_pids.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        !chrome_pids.is_empty(),
        "surf never started a browser process"
    );
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let mut offenders = Vec::new();
    for pid in chrome_pids.iter().chain(std::iter::once(&surf_pid)) {
        match listening_sockets(*pid) {
            Some(lines) if !lines.is_empty() => {
                offenders.push(format!("pid {pid}:\n{}", lines.join("\n")))
            }
            Some(_) => {}
            None => {
                eprintln!("lsof not available; skipping pipe_transport_opens_no_listening_port");
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
    eprintln!(
        "port gate: surf pid {surf_pid}, browser pid(s) {chrome_pids:?}: no listening sockets"
    );
    let out = child.wait_with_output().expect("surf exit");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        offenders.is_empty(),
        "listening TCP sockets while a browser is up over the pipe:\n{}",
        offenders.join("\n")
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

/// `lsof -iTCP -sTCP:LISTEN -a -p <pid>`: the listening sockets owned by
/// `pid` (without the header line). `None` if lsof is unavailable.
fn listening_sockets(pid: u32) -> Option<Vec<String>> {
    let out = Command::new("lsof")
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
