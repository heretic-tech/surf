//! Locate a Chrome / Chromium binary.
//!
//! [`find_chrome`] tries, in order:
//!
//! 1. the explicit `browser: path:` (a hard error if it does not exist);
//! 2. `SURF_CHROME` (same: set-but-missing is an error, not a fallback);
//! 3. `~/.cache/surf/chrome/*` — where `surf install` puts Chrome for
//!    Testing (newest version first);
//! 4. platform locations — macOS app bundles (Chrome, Chromium, Brave, Edge,
//!    Canary) in `/Applications` and `~/Applications`; Linux names on
//!    `PATH` (`google-chrome`, `google-chrome-stable`, `chromium`,
//!    `chromium-browser`, `brave-browser`, `microsoft-edge`) plus
//!    `/opt/google/chrome/chrome`; Windows Program Files / LocalAppData;
//! 5. other tools' caches: Playwright (`~/Library/Caches/ms-playwright`,
//!    `~/.cache/ms-playwright`, `%LOCALAPPDATA%\ms-playwright`), Puppeteer
//!    (`~/.cache/puppeteer`) and Apostate (`~/Library/Caches/apostate`,
//!    `~/.cache/apostate`) — found by a bounded walk for an executable named
//!    like a Chromium binary, so no tool's internal layout is hard-coded.
//!
//! The version is read with `<binary> --version` under a 3 s timeout. When
//! nothing is found the error lists every location that was checked.
//! Tests use [`chrome_or_skip`] and skip (printing a message) when it
//! returns `None`.

use crate::error::BrowserError;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long `--version` may take before we give up on it (the binary is
/// still used; only `version` is `None`).
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(3);

/// Executable names that identify a Chromium-family browser binary inside
/// a downloaded bundle (Playwright, Puppeteer, Chrome for Testing,
/// Apostate). Matched exactly against the file name.
const BUNDLE_BINARY_NAMES: &[&str] = &[
    "Google Chrome for Testing",
    "Google Chrome",
    "Chromium",
    "Apostate",
    "chrome",
    "chromium",
    "apostate",
    "chrome.exe",
    "chromium.exe",
    "apostate.exe",
];

/// Directories the bundle walk never descends into: they are large and
/// never contain the main binary.
const PRUNED_DIRS: &[&str] = &[
    "Frameworks",
    "Resources",
    "Libraries",
    "locales",
    "lib",
    "swiftshader",
    "node_modules",
    "widevinecdm",
    "ffmpeg",
];

/// Maximum directory depth below a cache root for the bundle walk.
const WALK_MAX_DEPTH: usize = 8;

/// Maximum directory entries one walk may visit (keeps a stray huge cache
/// from stalling discovery).
const WALK_MAX_ENTRIES: usize = 20_000;

/// Where a binary came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `browser: path:`.
    Explicit,
    /// `SURF_CHROME`.
    Env,
    /// `~/.cache/surf/chrome` (`surf install`).
    SurfCache,
    /// A platform default location or `PATH`.
    Platform,
    /// Playwright's browser cache.
    Playwright,
    /// Puppeteer's browser cache.
    Puppeteer,
    /// Apostate's cache.
    Apostate,
}

impl Origin {
    /// Short human label (`surf doctor`).
    pub fn label(self) -> &'static str {
        match self {
            Origin::Explicit => "browser.path",
            Origin::Env => "SURF_CHROME",
            Origin::SurfCache => "~/.cache/surf/chrome",
            Origin::Platform => "platform default",
            Origin::Playwright => "playwright cache",
            Origin::Puppeteer => "puppeteer cache",
            Origin::Apostate => "apostate cache",
        }
    }
}

/// A discovered browser binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The executable.
    pub path: PathBuf,
    /// Output of `--version` (e.g. `Google Chrome 131.0.6778.86`), or
    /// `None` if it failed or took longer than [`VERSION_TIMEOUT`].
    pub version: Option<String>,
    /// Which step found it.
    pub origin: Origin,
}

/// Find a Chrome binary; see the module docs for the order. `explicit` is
/// the `browser: path:` value.
pub fn find_chrome(explicit: Option<&Path>) -> Result<Found, BrowserError> {
    let mut tried: Vec<String> = Vec::new();

    if let Some(p) = explicit {
        if is_executable(p) {
            return Ok(found(p.to_path_buf(), Origin::Explicit));
        }
        return Err(BrowserError::Config {
            what: "browser.path".into(),
            reason: format!("{} is not an executable file", p.display()),
        });
    }

    if let Some(p) = std::env::var_os("SURF_CHROME") {
        let p = PathBuf::from(p);
        if is_executable(&p) {
            return Ok(found(p, Origin::Env));
        }
        return Err(BrowserError::Config {
            what: "SURF_CHROME".into(),
            reason: format!("{} is not an executable file", p.display()),
        });
    }
    tried.push("SURF_CHROME (unset)".into());

    if let Some(root) = surf_cache_root() {
        tried.push(format!(
            "{}/** ({})",
            root.display(),
            Origin::SurfCache.label()
        ));
        if let Some(p) = walk_for_binary(&root) {
            return Ok(found(p, Origin::SurfCache));
        }
    }

    for p in platform_candidates() {
        tried.push(p.display().to_string());
        if is_executable(&p) {
            return Ok(found(p, Origin::Platform));
        }
    }
    for name in path_names() {
        tried.push(format!("{name} on PATH"));
        if let Some(p) = which(name) {
            return Ok(found(p, Origin::Platform));
        }
    }

    for (origin, root) in tool_cache_roots() {
        tried.push(format!("{}/** ({})", root.display(), origin.label()));
        if let Some(p) = walk_for_binary(&root) {
            return Ok(found(p, origin));
        }
    }

    Err(BrowserError::NotFound { tried })
}

/// Every location discovery would check, in order, without checking them
/// (`surf doctor`). Cache roots are shown as `<root>/**`.
pub fn search_locations() -> Vec<String> {
    let mut v = vec!["SURF_CHROME".to_string()];
    if let Some(root) = surf_cache_root() {
        v.push(format!(
            "{}/** ({})",
            root.display(),
            Origin::SurfCache.label()
        ));
    }
    v.extend(
        platform_candidates()
            .iter()
            .map(|p| p.display().to_string()),
    );
    v.extend(path_names().iter().map(|n| format!("{n} on PATH")));
    for (origin, root) in tool_cache_roots() {
        v.push(format!("{}/** ({})", root.display(), origin.label()));
    }
    v
}

/// Helper for tests: returns the Chrome path or prints a skip message.
pub fn chrome_or_skip(test_name: &str) -> Option<PathBuf> {
    match find_chrome(None) {
        Ok(f) => Some(f.path),
        Err(e) => {
            eprintln!("skipping {test_name}: {e}");
            None
        }
    }
}

/// Minimal `which`: search `PATH` for an executable file named `name`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
}

/// Run `<path> --version` with a [`VERSION_TIMEOUT`] and return the first
/// non-empty line of stdout.
pub fn read_version(path: &Path) -> Option<String> {
    use std::io::Read;
    use std::process::{Command, Stdio};

    let mut child = Command::new(path)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Drain stdout on a thread so a chatty binary cannot block on the pipe
    // while we poll for exit.
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let deadline = Instant::now() + VERSION_TIMEOUT;
    let exited = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => break false,
        }
    };
    if !exited {
        tracing::warn!(
            "{} --version did not finish within {VERSION_TIMEOUT:?}",
            path.display()
        );
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let out = reader.join().ok()?;
    out.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

fn found(path: PathBuf, origin: Origin) -> Found {
    let version = read_version(&path);
    tracing::debug!(
        "chrome: {} ({}, {})",
        path.display(),
        origin.label(),
        version.as_deref().unwrap_or("version unknown")
    );
    Found {
        path,
        version,
        origin,
    }
}

/// `~/.cache/surf/chrome` — where `surf install` puts Chrome for Testing.
pub fn surf_cache_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".cache/surf/chrome"))
}

/// Other tools' browser caches, in search order.
fn tool_cache_roots() -> Vec<(Origin, PathBuf)> {
    let mut v = Vec::new();
    let home = dirs::home_dir();
    #[cfg(target_os = "macos")]
    if let Some(h) = &home {
        v.push((Origin::Playwright, h.join("Library/Caches/ms-playwright")));
    }
    #[cfg(target_os = "windows")]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        v.push((Origin::Playwright, Path::new(&local).join("ms-playwright")));
    }
    if let Some(h) = &home {
        v.push((Origin::Playwright, h.join(".cache/ms-playwright")));
        v.push((Origin::Puppeteer, h.join(".cache/puppeteer")));
    }
    #[cfg(target_os = "macos")]
    if let Some(h) = &home {
        v.push((Origin::Apostate, h.join("Library/Caches/apostate")));
    }
    if let Some(h) = &home {
        v.push((Origin::Apostate, h.join(".cache/apostate")));
    }
    v
}

/// Platform default binary locations (checked in order).
pub fn platform_candidates() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        const APPS: &[&str] = &[
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Chromium.app/Contents/MacOS/Chromium",
            "Brave Browser.app/Contents/MacOS/Brave Browser",
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        ];
        for app in APPS {
            v.push(Path::new("/Applications").join(app));
        }
        if let Some(home) = dirs::home_dir() {
            for app in APPS {
                v.push(home.join("Applications").join(app));
            }
        }
    }
    #[cfg(target_os = "linux")]
    {
        v.push(PathBuf::from("/opt/google/chrome/chrome"));
    }
    #[cfg(target_os = "windows")]
    {
        for base in [
            std::env::var_os("PROGRAMFILES"),
            std::env::var_os("PROGRAMFILES(X86)"),
            std::env::var_os("LOCALAPPDATA"),
        ]
        .into_iter()
        .flatten()
        {
            let base = Path::new(&base);
            v.push(base.join("Google/Chrome/Application/chrome.exe"));
            v.push(base.join("Chromium/Application/chrome.exe"));
            v.push(base.join("BraveSoftware/Brave-Browser/Application/brave.exe"));
            v.push(base.join("Microsoft/Edge/Application/msedge.exe"));
            v.push(base.join("Google/Chrome SxS/Application/chrome.exe"));
        }
    }
    v
}

/// Names looked up on `PATH` (after the platform locations).
fn path_names() -> &'static [&'static str] {
    #[cfg(target_os = "windows")]
    {
        &["chrome.exe", "chromium.exe", "msedge.exe"]
    }
    #[cfg(not(target_os = "windows"))]
    {
        &[
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "brave-browser",
            "microsoft-edge",
        ]
    }
}

/// Bounded walk below `root` for an executable whose file name is one of
/// [`BUNDLE_BINARY_NAMES`]. Sibling directories are visited newest-version
/// first (numeric-aware descending name order) so `chromium-1243` beats
/// `chromium-1208` and `155.0.1` beats `152.0.9`.
pub fn walk_for_binary(root: &Path) -> Option<PathBuf> {
    if !root.is_dir() {
        return None;
    }
    let mut budget = WALK_MAX_ENTRIES;
    walk(root, 0, &mut budget)
}

fn walk(dir: &Path, depth: usize, budget: &mut usize) -> Option<PathBuf> {
    if depth > WALK_MAX_DEPTH || *budget == 0 {
        return None;
    }
    let entries = std::fs::read_dir(dir).ok()?;
    let mut files = Vec::new();
    let mut dirs = Vec::new();
    for e in entries.flatten() {
        if *budget == 0 {
            break;
        }
        *budget -= 1;
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_file() {
            if BUNDLE_BINARY_NAMES.contains(&name) {
                files.push(e.path());
            }
        } else if ft.is_dir() && !name.starts_with('.') && !PRUNED_DIRS.contains(&name) {
            dirs.push((name.to_owned(), e.path()));
        }
    }
    // Prefer the most specific name first (Chrome for Testing before a
    // generic `chrome`), then executability.
    for want in BUNDLE_BINARY_NAMES {
        if let Some(p) = files
            .iter()
            .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(want))
        {
            if is_executable(p) {
                return Some(p.clone());
            }
        }
    }
    dirs.sort_by(|a, b| version_cmp(&b.0, &a.0));
    for (_, d) in dirs {
        if let Some(p) = walk(&d, depth + 1, budget) {
            return Some(p);
        }
    }
    None
}

/// Compare two names so that embedded numbers compare numerically
/// (`chromium-1243` > `chromium-1208`, `155.0.8059.31` > `152.0.7977.83`).
fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ai = a.chars().peekable();
    let mut bi = b.chars().peekable();
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na = take_number(&mut ai);
                let nb = take_number(&mut bi);
                if na != nb {
                    return na.cmp(&nb);
                }
            }
            (Some(x), Some(y)) => {
                ai.next();
                bi.next();
                if x != y {
                    return x.cmp(&y);
                }
            }
        }
    }
}

fn take_number(it: &mut std::iter::Peekable<std::str::Chars<'_>>) -> u64 {
    let mut n: u64 = 0;
    while let Some(c) = it.peek().copied() {
        if !c.is_ascii_digit() {
            break;
        }
        it.next();
        n = n.saturating_mul(10).saturating_add(c as u64 - '0' as u64);
    }
    n
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_file()
        && p.metadata()
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    #[test]
    fn version_order_is_numeric_aware() {
        assert_eq!(
            version_cmp("chromium-1243", "chromium-1208"),
            Ordering::Greater
        );
        assert_eq!(
            version_cmp("155.0.8059.31", "152.0.7977.83"),
            Ordering::Greater
        );
        assert_eq!(version_cmp("9", "10"), Ordering::Less);
        assert_eq!(version_cmp("a", "a"), Ordering::Equal);
        assert_eq!(
            version_cmp("mac_arm-128.0", "mac_arm-133.0"),
            Ordering::Less
        );
    }

    #[test]
    fn explicit_missing_path_is_a_hard_error() {
        let err = find_chrome(Some(Path::new("/definitely/not/here/chrome"))).unwrap_err();
        assert!(matches!(err, BrowserError::Config { .. }), "{err}");
        assert!(err.to_string().contains("browser.path"), "{err}");
    }

    #[test]
    fn walk_finds_bundle_binary_newest_first() {
        let root = tempfile::tempdir().unwrap();
        let mk = |ver: &str| {
            let dir = root
                .path()
                .join(ver)
                .join("chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS");
            std::fs::create_dir_all(&dir).unwrap();
            let bin = dir.join("Google Chrome for Testing");
            std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
            bin
        };
        let _old = mk("128.0.6613.119");
        let new = mk("133.0.6943.141");
        // A Frameworks dir with a decoy must be pruned.
        let decoy_dir = root.path().join("133.0.6943.141/Frameworks");
        std::fs::create_dir_all(&decoy_dir).unwrap();
        std::fs::write(decoy_dir.join("chrome"), b"").unwrap();
        assert_eq!(walk_for_binary(root.path()), Some(new));
        assert_eq!(walk_for_binary(&root.path().join("nope")), None);
    }

    #[test]
    fn not_found_message_lists_locations() {
        let e = BrowserError::NotFound {
            tried: vec!["/a/chrome".into(), "chromium on PATH".into()],
        };
        let s = e.to_string();
        assert!(s.contains("SURF_CHROME"), "{s}");
        assert!(s.contains("/a/chrome"), "{s}");
        assert!(s.contains("chromium on PATH"), "{s}");
    }

    #[test]
    fn search_locations_are_nonempty_and_mention_caches() {
        let locs = search_locations();
        assert!(locs.iter().any(|l| l.contains("surf/chrome")), "{locs:?}");
        assert!(locs.iter().any(|l| l.contains("puppeteer")), "{locs:?}");
    }
}
