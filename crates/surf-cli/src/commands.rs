//! Subcommand implementations.

use crate::RunOpts;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use surf_runtime::{is_no_browser, RunError, Runtime, RuntimeOptions};

/// Exit code for syntax errors.
pub const EXIT_SYNTAX: i32 = 2;
/// Exit code for runtime errors.
pub const EXIT_RUNTIME: i32 = 1;
/// Exit code when no browser can be found.
pub const EXIT_NO_BROWSER: i32 = 3;

/// `30s`, `500ms`, `2m`, `1h`, `1.5s`, or a bare number of seconds.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num
        .parse()
        .map_err(|_| format!("{s:?} is not a duration (try 30s, 500ms, 2m)"))?;
    let secs = match unit.trim() {
        "" | "s" => n,
        "ms" => n / 1000.0,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        u => return Err(format!("unknown duration unit {u:?} (use ms, s, m, h)")),
    };
    if secs.is_nan() || secs < 0.0 {
        return Err(format!("{s:?} is not a duration"));
    }
    Ok(Duration::from_secs_f64(secs))
}

/// Runtime options from the CLI flags.
pub fn options(opts: &RunOpts, chrome: Option<PathBuf>, trace_cdp: bool) -> RuntimeOptions {
    RuntimeOptions {
        trace_cdp,
        chrome_path: chrome,
        headless: if opts.headless {
            Some(true)
        } else if opts.headed {
            Some(false)
        } else {
            None
        },
        json: opts.json,
        timeout: opts.timeout,
        color: use_color(),
    }
}

fn read_source(file: &Path) -> Result<(String, String), i32> {
    if file.as_os_str() == "-" {
        let mut s = String::new();
        if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut s) {
            eprintln!("surf: cannot read stdin: {e}");
            return Err(EXIT_RUNTIME);
        }
        return Ok(("<stdin>".to_string(), s));
    }
    match std::fs::read_to_string(file) {
        Ok(s) => Ok((file.display().to_string(), s)),
        Err(e) => {
            eprintln!("surf: cannot read {}: {e}", file.display());
            Err(EXIT_RUNTIME)
        }
    }
}

/// Exit code for a runtime failure.
fn runtime_exit_code(e: &surf_vm::RuntimeError) -> i32 {
    if is_no_browser(e) {
        EXIT_NO_BROWSER
    } else {
        EXIT_RUNTIME
    }
}

/// `surf run`.
pub async fn run(file: PathBuf, opts: RuntimeOptions) -> i32 {
    let (name, source) = match read_source(&file) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let color = opts.color;
    let rt = Runtime::new(opts);
    match rt.run(&name, &source).await {
        Ok(code) => code,
        Err(RunError::Syntax(d)) => {
            eprint!("{}", d.render(&source, color));
            EXIT_SYNTAX
        }
        Err(RunError::Runtime(e)) => {
            eprint!("{}", rt.render_error(&e));
            runtime_exit_code(&e)
        }
    }
}

/// `surf check`.
pub async fn check(file: PathBuf) -> i32 {
    let (name, source) = match read_source(&file) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let program = match surf_syntax::parse(&name, &source) {
        Ok(p) => p,
        Err(d) => {
            eprint!("{}", d.render(&source, use_color()));
            return EXIT_SYNTAX;
        }
    };
    if let Err(e) = surf_vm::compile(&name, &program) {
        eprint!("{}", e.into_diagnostics(&name).render(&source, use_color()));
        return EXIT_SYNTAX;
    }
    println!("{name}: ok");
    0
}

/// `surf doctor`: discovery, display, Xvfb (Linux), a timed headless pipe
/// launch (the transport smoke test: spawn → first `Browser.getVersion`
/// answered) with the exact flag list, the shutdown ladder timing, and
/// with `--detector` the local detector page's results.
pub async fn doctor(chrome: Option<PathBuf>, detector: bool) -> i32 {
    use surf_browser::discovery::find_chrome;
    use surf_browser::display::has_display;
    use surf_browser::LaunchOptions;

    println!("surf {}", env!("CARGO_PKG_VERSION"));
    println!(
        "platform: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let found = match find_chrome(chrome.as_deref()) {
        Ok(f) => f,
        Err(e) => {
            println!("chrome:   not found");
            println!("{e}");
            println!("hint:     run `surf install` or set SURF_CHROME=/path/to/chrome");
            return EXIT_NO_BROWSER;
        }
    };
    println!(
        "chrome:   {} ({})",
        found.path.display(),
        found.origin.label()
    );
    println!(
        "version:  {}",
        found
            .version
            .as_deref()
            .unwrap_or("unknown (--version failed)")
    );
    let display = if cfg!(any(target_os = "macos", target_os = "windows")) {
        "yes (native window system)".to_string()
    } else if has_display() {
        format!(
            "yes ({})",
            std::env::var("DISPLAY")
                .map(|d| format!("DISPLAY={d}"))
                .or_else(
                    |_| std::env::var("WAYLAND_DISPLAY").map(|d| format!("WAYLAND_DISPLAY={d}"))
                )
                .unwrap_or_default()
        )
    } else {
        "none — scripts default to headless (or `virtual: true` for Xvfb)".to_string()
    };
    println!("display:  {display}");
    if cfg!(target_os = "linux") {
        match surf_browser::discovery::which("Xvfb") {
            Some(p) => println!("xvfb:     {}", p.display()),
            None => println!("xvfb:     not found (apt install xvfb for `virtual: true`)"),
        }
    }

    let opts = LaunchOptions {
        path: Some(found.path.clone()),
        headless: Some(true),
        ..Default::default()
    };
    let cfg = match opts.resolve() {
        Ok(c) => c,
        Err(e) => {
            println!("launch:   cannot build a launch config: {e}");
            return EXIT_RUNTIME;
        }
    };
    let t0 = Instant::now();
    let launched = match surf_browser::launch(cfg).await {
        Ok(l) => l,
        Err(e) => {
            println!(
                "launch:   FAILED after {} ms: {e}",
                t0.elapsed().as_millis()
            );
            return EXIT_RUNTIME;
        }
    };
    let launch_ms = t0.elapsed().as_millis();
    println!(
        "launch:   ok — {} (pid {})",
        launched.product,
        launched.pid()
    );
    println!(
        "transport: pipe (fd 3 / fd 4) — spawn → first Browser.getVersion answered in {launch_ms} ms"
    );
    let roundtrip = Instant::now();
    let version = launched
        .root()
        .with_timeout(Some(Duration::from_secs(5)))
        .call_raw("Browser.getVersion", serde_json::json!({}))
        .await;
    match version {
        Ok(v) => println!(
            "cdp:      Browser.getVersion round trip {} ms (protocol {}, {})",
            roundtrip.elapsed().as_millis(),
            v["protocolVersion"].as_str().unwrap_or("?"),
            v["userAgent"].as_str().unwrap_or("?")
        ),
        Err(e) => println!("cdp:      Browser.getVersion failed: {e}"),
    }
    println!("flags:    {}", launched.args.join(" "));
    println!(
        "profile:  {} (temporary, removed on close)",
        launched.profile_dir.path().display()
    );
    let t1 = Instant::now();
    launched.close().await;
    println!(
        "close:    shutdown ladder finished in {} ms",
        t1.elapsed().as_millis()
    );
    println!("quiet:    no --enable-automation; only Page.enable is sent per page; never Runtime.enable / DOM.enable");
    if detector {
        return run_detector(found.path).await;
    }
    0
}

/// The local detector page, embedded so `surf doctor --detector` works
/// from an installed binary (the e2e suite serves the same file at
/// `/detector`).
const DETECTOR_HTML: &str = include_str!("../../../tools/detector/index.html");

/// `surf doctor --detector`: launch a browser the way a script would
/// (headed when a display exists), load the detector from a temp file,
/// interact with it (type, hover, click — so a main-world injection by any
/// action would show), and print every `<li data-check>` result. Exit 1
/// when a non-informational check fails.
async fn run_detector(chrome: PathBuf) -> i32 {
    use surf_browser::{ActionOptions, Browser, LaunchOptions, WaitUntil};

    let opts = LaunchOptions {
        path: Some(chrome),
        ..Default::default()
    };
    let headless = opts.headless_decision();
    let mode = if headless { "headless" } else { "headed" };
    let file = std::env::temp_dir().join(format!("surf-detector-{}.html", std::process::id()));
    if let Err(e) = std::fs::write(&file, DETECTOR_HTML) {
        println!("detector: cannot write {}: {e}", file.display());
        return EXIT_RUNTIME;
    }
    let url = format!("file://{}?mode={mode}", file.display());
    let outcome = async {
        let browser = Browser::launch(opts).await?;
        let result = async {
            let page = browser.sole_page().await?;
            page.set_timeout(Duration::from_secs(15));
            page.goto(&url, WaitUntil::Load).await?;
            page.wait("#done", ActionOptions::default()).await?;
            page.type_text("#probe", "surf doctor", ActionOptions::default())
                .await?;
            page.hover("#recheck", ActionOptions::default()).await?;
            page.click("#recheck", ActionOptions::default()).await?;
            page.wait("#done[data-round='2']", ActionOptions::default())
                .await?;
            let mut rows = Vec::new();
            for li in page.all("li[data-check]").await? {
                rows.push((
                    li.attr("data-check").await?.unwrap_or_default(),
                    li.attr("data-status").await?.unwrap_or_default(),
                    li.attr("data-detail").await?.unwrap_or_default(),
                ));
            }
            let summary = page.text("#summary", ActionOptions::default()).await?;
            Ok::<_, surf_browser::BrowserError>((rows, summary))
        }
        .await;
        let _ = browser.close().await;
        result
    }
    .await;
    let _ = std::fs::remove_file(&file);
    match outcome {
        Ok((rows, summary)) => {
            println!("detector: {mode} — {summary}");
            let width = rows.iter().map(|r| r.0.len()).max().unwrap_or(0);
            let mut failed = 0;
            for (name, status, detail) in &rows {
                println!("  {status:<4} {name:<width$}  {detail}");
                if status == "FAIL" {
                    failed += 1;
                }
            }
            if failed > 0 {
                EXIT_RUNTIME
            } else {
                0
            }
        }
        Err(e) => {
            println!("detector: FAILED: {e}");
            EXIT_RUNTIME
        }
    }
}

/// `surf repl`: one chunk at a time on a single runtime. A line ending in
/// `:` starts a block that continues until an empty line. Variables do
/// not persist between chunks yet (TASKS.md); functions, `browser:`
/// blocks, handlers, the browser and its pages do.
pub async fn repl(opts: RuntimeOptions) -> i32 {
    use std::io::Write;
    use tokio::io::AsyncBufReadExt;

    let rt = Runtime::new(opts);
    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin());
    if interactive {
        eprintln!("surf repl — one line at a time; a line ending in `:` opens a block (finish with an empty line); `.exit` to quit");
    }
    let mut buffer = String::new();
    let mut chunk = 0usize;
    let mut code = 0;
    loop {
        if interactive {
            let prompt = if buffer.is_empty() {
                "surf> "
            } else {
                "  ... "
            };
            eprint!("{prompt}");
            let _ = std::io::stderr().flush();
        }
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => {
                eprintln!("surf: stdin: {e}");
                break;
            }
        };
        if buffer.is_empty() {
            let trimmed = line.trim();
            if trimmed == ".exit" || trimmed == ".quit" {
                break;
            }
            if trimmed.is_empty() {
                continue;
            }
        }
        let opens_block = line.trim_end().ends_with(':');
        let continuing = !buffer.is_empty();
        if opens_block || (continuing && !line.trim().is_empty()) {
            buffer.push_str(&line);
            buffer.push('\n');
            continue;
        }
        let source = if continuing {
            std::mem::take(&mut buffer)
        } else {
            format!("{line}\n")
        };
        chunk += 1;
        let name = format!("<repl:{chunk}>");
        match rt.exec(&name, &source).await {
            Ok(None) => {}
            Ok(Some(c)) => {
                code = c;
                break;
            }
            Err(RunError::Syntax(d)) => eprint!("{}", d.render(&source, rt.options().color)),
            Err(RunError::Runtime(e)) => eprint!("{}", rt.render_error(&e)),
        }
    }
    if !buffer.is_empty() {
        chunk += 1;
        let name = format!("<repl:{chunk}>");
        match rt.exec(&name, &buffer).await {
            Ok(Some(c)) => code = c,
            Ok(None) => {}
            Err(RunError::Syntax(d)) => eprint!("{}", d.render(&buffer, rt.options().color)),
            Err(RunError::Runtime(e)) => eprint!("{}", rt.render_error(&e)),
        }
    }
    rt.shutdown().await;
    code
}

/// Chrome for Testing's "last known good versions" manifest.
const CFT_MANIFEST: &str =
    "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json";

/// `surf install`: download the stable Chrome for Testing build for this
/// platform into `~/.cache/surf/chrome/<version>/` and verify it with
/// `--version`. Uses the system `curl` and `unzip` (`tar` on Windows) so
/// the binary carries no HTTP client.
pub async fn install(force: bool) -> i32 {
    use surf_browser::discovery::{read_version, surf_cache_root, walk_for_binary};

    let platform = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "mac-arm64",
        ("macos", "x86_64") => "mac-x64",
        ("linux", "x86_64") => "linux64",
        ("windows", "x86_64") => "win64",
        ("windows", "x86") => "win32",
        (os, arch) => {
            eprintln!("surf install: no Chrome for Testing build for {os}/{arch}");
            return EXIT_RUNTIME;
        }
    };
    let Some(root) = surf_cache_root() else {
        eprintln!("surf install: cannot determine the home directory");
        return EXIT_RUNTIME;
    };
    eprintln!("fetching {CFT_MANIFEST}");
    let manifest = match curl(&["-fsSL", CFT_MANIFEST]).await {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("surf install: {e}");
            return EXIT_RUNTIME;
        }
    };
    let manifest: serde_json::Value = match serde_json::from_slice(&manifest) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("surf install: manifest is not JSON: {e}");
            return EXIT_RUNTIME;
        }
    };
    let stable = &manifest["channels"]["Stable"];
    let Some(version) = stable["version"].as_str() else {
        eprintln!("surf install: manifest has no Stable version");
        return EXIT_RUNTIME;
    };
    let url = stable["downloads"]["chrome"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|d| d["platform"] == platform)
                .and_then(|d| d["url"].as_str())
        })
        .map(str::to_owned);
    let Some(url) = url else {
        eprintln!("surf install: no {platform} download for Chrome {version}");
        return EXIT_RUNTIME;
    };
    let dest = root.join(version);
    if !force {
        if let Some(bin) = walk_for_binary(&dest) {
            println!(
                "already installed: {} ({})",
                bin.display(),
                read_version(&bin).unwrap_or_else(|| "version unknown".into())
            );
            return 0;
        }
    }
    if let Err(e) = std::fs::create_dir_all(&dest) {
        eprintln!("surf install: cannot create {}: {e}", dest.display());
        return EXIT_RUNTIME;
    }
    let zip = dest.join("chrome.zip");
    eprintln!("downloading Chrome for Testing {version} ({platform})\n  {url}");
    let zip_s = zip.to_string_lossy().into_owned();
    if let Err(e) = curl(&["-fL", "--progress-bar", "-o", &zip_s, &url]).await {
        eprintln!("surf install: download failed: {e}");
        let _ = std::fs::remove_file(&zip);
        return EXIT_RUNTIME;
    }
    eprintln!("unpacking into {}", dest.display());
    let dest_s = dest.to_string_lossy().into_owned();
    let unpack = if cfg!(windows) {
        tokio::process::Command::new("tar")
            .args(["-xf", &zip_s, "-C", &dest_s])
            .status()
            .await
    } else {
        tokio::process::Command::new("unzip")
            .args(["-q", "-o", &zip_s, "-d", &dest_s])
            .status()
            .await
    };
    let _ = std::fs::remove_file(&zip);
    match unpack {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("surf install: unpack failed ({s})");
            return EXIT_RUNTIME;
        }
        Err(e) => {
            eprintln!("surf install: cannot run the unpacker: {e}");
            return EXIT_RUNTIME;
        }
    }
    let Some(bin) = walk_for_binary(&dest) else {
        eprintln!(
            "surf install: unpacked, but no Chrome binary found under {}",
            dest.display()
        );
        return EXIT_RUNTIME;
    };
    match read_version(&bin) {
        Some(v) => {
            println!("installed {} — {v}", bin.display());
            println!("discovery finds it automatically (after SURF_CHROME and browser.path)");
            0
        }
        None => {
            eprintln!(
                "surf install: {} does not run (`--version` failed)",
                bin.display()
            );
            EXIT_RUNTIME
        }
    }
}

async fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = tokio::process::Command::new("curl")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| format!("cannot run curl: {e} (surf install needs curl on PATH)"))?;
    if !out.status.success() {
        return Err(format!(
            "curl failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

fn use_color() -> bool {
    std::env::var_os("NO_COLOR").is_none() && std::io::IsTerminal::is_terminal(&std::io::stderr())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30s").unwrap(), Duration::from_secs(30));
        assert_eq!(parse_duration("500ms").unwrap(), Duration::from_millis(500));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(parse_duration("1.5s").unwrap(), Duration::from_millis(1500));
        assert_eq!(parse_duration("7").unwrap(), Duration::from_secs(7));
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("3d").is_err());
    }
}
