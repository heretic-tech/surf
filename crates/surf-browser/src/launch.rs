//! Launch options and the exact flag set Surf passes.
//!
//! **Quiet rule 2.** Surf passes, in this order and nothing else:
//! `--remote-debugging-pipe` (or `--remote-debugging-port=N` when the user
//! asked for a port), `--user-data-dir=<dir>` (always — Chrome ≥ 136
//! silently ignores pipe/port on the default profile), `--no-first-run`,
//! `--no-default-browser-check`, `--window-size=W,H`, `--headless` (only
//! when headless), `--proxy-server=<scheme://host:port>` (credentials
//! stripped; auth is answered via `Fetch.authRequired`), the user's
//! `flags`, then `about:blank`.
//!
//! Never: `--enable-automation`, `--disable-blink-features=AutomationControlled`,
//! or any `--disable-*` the user did not write themselves.

use std::path::PathBuf;
use std::time::Duration;

/// How the runtime talks to Chrome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdpMode {
    /// `--remote-debugging-pipe` (default).
    Pipe,
    /// `--remote-debugging-port=N` + websocket (user wrote `cdp: 9222`).
    Port(u16),
    /// Attach to a running browser / remote provider (`cdp: "ws://…"`,
    /// `pool: "wss://…"`). Nothing is launched.
    Attach(String),
}

/// Everything needed to start a browser. Built by `surf-runtime` from the
/// `browser:` block.
#[derive(Debug, Clone)]
pub struct LaunchOptions {
    /// Binary path (`None` → discovery).
    pub path: Option<PathBuf>,
    /// Transport mode.
    pub cdp: CdpMode,
    /// Headless (`None` → headed if a display exists).
    pub headless: Option<bool>,
    /// Linux: manage Xvfb.
    pub virtual_display: bool,
    /// Window size.
    pub size: (u32, u32),
    /// Persistent profile dir (`None` → fresh temp dir, deleted on exit).
    pub profile: Option<PathBuf>,
    /// Browser-level proxy URL (may contain credentials; they are stripped
    /// from the flag and answered via `Fetch.authRequired`).
    pub proxy: Option<String>,
    /// Rotation list for `shift_proxy()`.
    pub proxies: Vec<String>,
    /// User-supplied flags, passed verbatim.
    pub flags: Vec<String>,
    /// Default action timeout.
    pub timeout: Duration,
    /// `chrome` (default) or `apostate` (reserved; clear "not yet" error).
    pub engine: String,
}

impl Default for LaunchOptions {
    fn default() -> Self {
        Self {
            path: None,
            cdp: CdpMode::Pipe,
            headless: None,
            virtual_display: false,
            size: (1280, 800),
            profile: None,
            proxy: None,
            proxies: Vec::new(),
            flags: Vec::new(),
            timeout: Duration::from_secs(30),
            engine: "chrome".into(),
        }
    }
}

impl LaunchOptions {
    /// Build the full argument vector for `user_data_dir` and the resolved
    /// `headless` decision. This is the single place flags are decided.
    pub fn args(&self, user_data_dir: &std::path::Path, headless: bool) -> Vec<String> {
        let mut a = Vec::new();
        match &self.cdp {
            CdpMode::Pipe => a.push("--remote-debugging-pipe".to_string()),
            CdpMode::Port(p) => a.push(format!("--remote-debugging-port={p}")),
            CdpMode::Attach(_) => {}
        }
        a.push(format!("--user-data-dir={}", user_data_dir.display()));
        a.push("--no-first-run".to_string());
        a.push("--no-default-browser-check".to_string());
        a.push(format!("--window-size={},{}", self.size.0, self.size.1));
        if headless {
            a.push("--headless".to_string());
        }
        if let Some(proxy) = &self.proxy {
            a.push(format!("--proxy-server={}", strip_credentials(proxy)));
        }
        a.extend(self.flags.iter().cloned());
        a.push("about:blank".to_string());
        a
    }
}

/// Remove `user:pass@` from a proxy URL.
pub fn strip_credentials(url: &str) -> String {
    match (url.find("://"), url.rfind('@')) {
        (Some(scheme_end), Some(at)) if at > scheme_end => {
            format!("{}{}", &url[..scheme_end + 3], &url[at + 1..])
        }
        (None, Some(at)) => url[at + 1..].to_string(),
        _ => url.to_string(),
    }
}

/// Extract `(user, pass)` from a proxy URL, if present.
pub fn proxy_credentials(url: &str) -> Option<(String, String)> {
    let rest = url.find("://").map(|i| &url[i + 3..]).unwrap_or(url);
    let at = rest.rfind('@')?;
    let creds = &rest[..at];
    let (u, p) = creds.split_once(':').unwrap_or((creds, ""));
    Some((u.to_string(), p.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_are_quiet() {
        let opts = LaunchOptions {
            proxy: Some("http://u:p@proxy:8080".into()),
            flags: vec!["--lang=en-US".into()],
            ..Default::default()
        };
        let args = opts.args(std::path::Path::new("/tmp/x"), false);
        assert_eq!(
            args,
            vec![
                "--remote-debugging-pipe",
                "--user-data-dir=/tmp/x",
                "--no-first-run",
                "--no-default-browser-check",
                "--window-size=1280,800",
                "--proxy-server=http://proxy:8080",
                "--lang=en-US",
                "about:blank",
            ]
        );
        for a in &args {
            assert!(!a.contains("enable-automation"), "{a}");
            assert!(!a.starts_with("--disable-"), "{a}");
        }
        assert_eq!(
            proxy_credentials("http://u:p@proxy:8080"),
            Some(("u".into(), "p".into()))
        );
    }

    #[test]
    fn headless_flag_only_when_headless() {
        let opts = LaunchOptions::default();
        let args = opts.args(std::path::Path::new("/tmp/x"), true);
        assert!(args.contains(&"--headless".to_string()));
        let args = opts.args(std::path::Path::new("/tmp/x"), false);
        assert!(!args.contains(&"--headless".to_string()));
    }
}
