//! Locate a Chrome / Chromium binary.
//!
//! Order: `SURF_CHROME` env → explicit `path` → platform defaults. Tests use
//! [`find_chrome`] and skip (printing a message) when it returns `None`.

use std::path::{Path, PathBuf};

/// Candidate binary locations per platform (checked in order).
pub fn default_candidates() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        for p in [
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
            "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
            "/Applications/Chromium.app/Contents/MacOS/Chromium",
            "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
        ] {
            v.push(PathBuf::from(p));
        }
        if let Some(home) = dirs::home_dir() {
            v.push(home.join("Applications/Google Chrome.app/Contents/MacOS/Google Chrome"));
        }
    }
    #[cfg(target_os = "linux")]
    {
        for p in [
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/snap/bin/chromium",
            "/opt/google/chrome/chrome",
            "/usr/bin/microsoft-edge",
            "/usr/bin/brave-browser",
        ] {
            v.push(PathBuf::from(p));
        }
    }
    #[cfg(target_os = "windows")]
    {
        for base in [
            std::env::var("PROGRAMFILES").ok(),
            std::env::var("PROGRAMFILES(X86)").ok(),
            std::env::var("LOCALAPPDATA").ok(),
        ]
        .into_iter()
        .flatten()
        {
            v.push(Path::new(&base).join("Google/Chrome/Application/chrome.exe"));
            v.push(Path::new(&base).join("Chromium/Application/chrome.exe"));
            v.push(Path::new(&base).join("Microsoft/Edge/Application/msedge.exe"));
        }
    }
    v
}

/// Find a Chrome binary: `SURF_CHROME`, then `PATH` names, then
/// [`default_candidates`]. Returns `None` if nothing exists.
pub fn find_chrome() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("SURF_CHROME") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    for name in [
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
        "chrome",
    ] {
        if let Some(p) = which(name) {
            return Some(p);
        }
    }
    default_candidates().into_iter().find(|p| p.is_file())
}

/// Minimal `which`: search `PATH` for an executable file named `name`.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| is_executable(p))
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

/// Helper for tests: returns the Chrome path or prints a skip message.
pub fn chrome_or_skip(test_name: &str) -> Option<PathBuf> {
    let found = find_chrome();
    if found.is_none() {
        eprintln!("skipping {test_name}: no Chrome found (set SURF_CHROME=/path/to/chrome)");
    }
    found
}
