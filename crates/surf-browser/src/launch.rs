//! Quiet launch: the exact flag set, the pipe, profiles, crash detection,
//! and the shutdown ladder.
//!
//! **Quiet rule 2.** Surf passes, in this order and nothing else
//! ([`LaunchConfig::args`] is the single place this is decided):
//! `--remote-debugging-pipe` (or `--remote-debugging-port=N` when the user
//! wrote `cdp: N`; on Windows the pipe flag is
//! `--remote-debugging-io-pipes=<r>,<w>`), `--user-data-dir=<dir>` (always —
//! Chrome ≥ 136 silently ignores pipe/port on the default profile),
//! `--no-first-run`, `--no-default-browser-check`,
//! `--disable-blink-features=AutomationControlled` (DECISIONS.md #12: Chrome
//! turns `navigator.webdriver` on merely because a debugger pipe/port is
//! configured; this undoes that one side effect — launched browsers only),
//! `--window-size=W,H`, `--headless` (only when headless),
//! `--proxy-server=<scheme://host:port>` (credentials stripped; they are
//! kept on [`Launched::proxy_credentials`] for the `Fetch.authRequired`
//! handler), the user's `flags` verbatim, then `about:blank`.
//!
//! Never: `--enable-automation`, or any other `--disable-*` the user did
//! not write themselves.
//!
//! [`launch`] builds the pipe pair (`surf_cdp::transport::pipe::create_pair`),
//! spawns Chrome with the child ends on fd 3 / fd 4 (unix `pre_exec` +
//! `dup2`), drains stderr into a 64-line ring buffer, waits for the first
//! `Browser.getVersion` (10 s), and returns a [`Launched`] whose
//! [`close`](Launched::close) runs the ladder `Browser.close` → 2 s →
//! `SIGTERM` → 2 s → `SIGKILL`, removes a temporary profile and stops the
//! virtual display. A process watcher marks the connection crashed
//! (`CdpError::BrowserCrashed { exit_code, stderr_tail }`) when Chrome exits
//! on its own.

use crate::discovery::find_chrome;
use crate::display::{has_display, VirtualDisplay};
use crate::error::BrowserError;
use futures::future::BoxFuture;
use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use surf_cdp::protocol::browser;
use surf_cdp::transport::pipe;
use surf_cdp::{Connection, Session, Transport};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, ChildStderr, Command};
use tokio::sync::{mpsc, watch};

/// How long Chrome may take to answer the first `Browser.getVersion`.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
/// Grace period at each rung of the shutdown ladder.
pub const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// How long to wait for the process after `SIGKILL`.
const KILL_WAIT: Duration = Duration::from_secs(5);
/// After the pipe closes, how long the driver waits for the exit status so
/// in-flight calls can fail with `BrowserCrashed` instead of `Closed`.
const EXIT_GRACE: Duration = Duration::from_secs(1);
/// Lines of stderr kept for diagnostics.
pub const STDERR_TAIL_LINES: usize = 64;
/// One `/json/version` + websocket attempt in port mode.
const PORT_CONNECT_ATTEMPT: Duration = Duration::from_secs(3);
/// The pipe flag on unix.
const PIPE_FLAG: &str = "--remote-debugging-pipe";
/// The one permitted `--disable-*` switch (DECISIONS.md #12).
pub const AUTOMATION_CONTROLLED_OFF: &str = "--disable-blink-features=AutomationControlled";

/// How the runtime talks to Chrome (the `cdp:` option).
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

/// Everything the `browser:` block can say. Built by `surf-runtime`;
/// resolved into a [`LaunchConfig`] by [`LaunchOptions::resolve`] when the
/// first action needs a browser.
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
    /// `downloads: dir` — allow downloads into this directory
    /// (`Browser.setDownloadBehavior`) and track them for `wait_download()`.
    pub downloads: Option<PathBuf>,
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
            downloads: None,
        }
    }
}

impl LaunchOptions {
    /// Resolve into a launch-ready config: run discovery (unless `path` is
    /// set), decide headed/headless (`headless: None` → headed when a
    /// display exists or `virtual: true` will provide one), parse the proxy.
    /// Fails for `engine: apostate` and for attach mode (nothing to launch).
    pub fn resolve(&self) -> Result<LaunchConfig, BrowserError> {
        if self.engine == "apostate" {
            return Err(BrowserError::Unsupported(
                "engine: apostate is not available yet — use engine: chrome".into(),
            ));
        }
        if self.engine != "chrome" {
            return Err(BrowserError::Config {
                what: "browser.engine".into(),
                reason: format!("unknown engine {:?} (expected chrome)", self.engine),
            });
        }
        let found = find_chrome(self.path.as_deref())?;
        let headless = self.headless_decision();
        self.resolve_with(found.path, headless)
    }

    /// Headed if a display exists (or Xvfb will be started), unless the
    /// user said otherwise.
    pub fn headless_decision(&self) -> bool {
        match self.headless {
            Some(h) => h,
            None => !(has_display() || (self.virtual_display && cfg!(target_os = "linux"))),
        }
    }

    /// [`resolve`](Self::resolve) with the binary and headless decision
    /// already made (no discovery, no display probe).
    pub fn resolve_with(
        &self,
        path: PathBuf,
        headless: bool,
    ) -> Result<LaunchConfig, BrowserError> {
        let transport = match &self.cdp {
            CdpMode::Pipe => TransportChoice::Pipe,
            CdpMode::Port(p) => TransportChoice::Port(*p),
            CdpMode::Attach(url) => {
                return Err(BrowserError::Config {
                    what: "browser.cdp".into(),
                    reason: format!("{url:?} attaches to a running browser; nothing to launch"),
                })
            }
        };
        let proxy = self.proxy.as_deref().map(ProxySpec::parse).transpose()?;
        if self.size.0 == 0 || self.size.1 == 0 {
            return Err(BrowserError::Config {
                what: "browser.size".into(),
                reason: format!(
                    "{}x{} is not a usable window size",
                    self.size.0, self.size.1
                ),
            });
        }
        Ok(LaunchConfig {
            path,
            headless,
            size: self.size,
            proxy,
            profile: self.profile.clone(),
            flags: self.flags.clone(),
            env: Vec::new(),
            virtual_display: self.virtual_display,
            transport,
            correct_automation_controlled: true,
        })
    }
}

/// Which debugging transport to ask Chrome for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportChoice {
    /// `--remote-debugging-pipe` (default; no listening socket).
    Pipe,
    /// `--remote-debugging-port=N` (`0` → Chrome picks; read from
    /// `DevToolsActivePort`).
    Port(u16),
}

/// Proxy credentials (never put on the command line).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    /// User name.
    pub username: String,
    /// Password (may be empty).
    pub password: String,
}

/// A parsed proxy URL: `scheme://[user:pass@]host[:port]` or `host:port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxySpec {
    /// `http` (default), `https`, `socks4`, `socks5`, …
    pub scheme: String,
    /// Host name or IP literal (IPv6 keeps its brackets).
    pub host: String,
    /// Port, if given.
    pub port: Option<u16>,
    /// `user:pass@` part, percent-decoded.
    pub credentials: Option<Credentials>,
}

impl ProxySpec {
    /// Parse `"http://user:pass@host:8080"`, `"socks5://host:1080"` or
    /// `"host:8080"`.
    pub fn parse(s: &str) -> Result<ProxySpec, BrowserError> {
        let err = |reason: String| BrowserError::Config {
            what: "browser.proxy".into(),
            reason,
        };
        let s = s.trim();
        if s.is_empty() {
            return Err(err("empty proxy URL".into()));
        }
        let (scheme, rest) = match s.split_once("://") {
            Some((sc, rest)) => (sc.to_ascii_lowercase(), rest),
            None => ("http".to_string(), s),
        };
        let rest = rest.trim_end_matches('/');
        let (creds, hostport) = match rest.rfind('@') {
            Some(at) => (Some(&rest[..at]), &rest[at + 1..]),
            None => (None, rest),
        };
        if hostport.is_empty() {
            return Err(err(format!("{s:?} has no host")));
        }
        let (host, port) = if let Some(stripped) = hostport.strip_prefix('[') {
            // IPv6 literal.
            let close = stripped
                .find(']')
                .ok_or_else(|| err(format!("{s:?}: unterminated IPv6 literal")))?;
            let host = format!("[{}]", &stripped[..close]);
            let after = &stripped[close + 1..];
            match after.strip_prefix(':') {
                Some(p) => (host, Some(p)),
                None if after.is_empty() => (host, None),
                None => return Err(err(format!("{s:?}: unexpected {after:?} after host"))),
            }
        } else {
            match hostport.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (hostport.to_string(), None),
            }
        };
        if host.is_empty() {
            return Err(err(format!("{s:?} has no host")));
        }
        let port = port
            .map(|p| {
                p.parse::<u16>()
                    .map_err(|_| err(format!("{s:?}: bad port {p:?}")))
            })
            .transpose()?;
        let credentials = creds.map(|c| {
            let (u, p) = c.split_once(':').unwrap_or((c, ""));
            Credentials {
                username: percent_decode(u),
                password: percent_decode(p),
            }
        });
        Ok(ProxySpec {
            scheme,
            host,
            port,
            credentials,
        })
    }

    /// The `--proxy-server=` value: `scheme://host[:port]`, no credentials.
    pub fn server_arg(&self) -> String {
        match self.port {
            Some(p) => format!("{}://{}:{}", self.scheme, self.host, p),
            None => format!("{}://{}", self.scheme, self.host),
        }
    }
}

/// Decode `%XX` escapes (credentials in proxy URLs). Invalid escapes are
/// kept verbatim.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

/// A launch-ready configuration: binary, resolved headless decision,
/// parsed proxy. Built from [`LaunchOptions::resolve`] or by hand.
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    /// The binary to run.
    pub path: PathBuf,
    /// Pass `--headless`.
    pub headless: bool,
    /// `--window-size=W,H`.
    pub size: (u32, u32),
    /// Browser-level proxy.
    pub proxy: Option<ProxySpec>,
    /// Persistent `profile:` dir (`None` → temp dir, removed on close).
    pub profile: Option<PathBuf>,
    /// User flags, verbatim, after Surf's own.
    pub flags: Vec<String>,
    /// Extra environment for the child (`DISPLAY` is added by Xvfb).
    pub env: Vec<(String, String)>,
    /// Linux: start Xvfb when no display is available.
    pub virtual_display: bool,
    /// Pipe (default) or port.
    pub transport: TransportChoice,
    /// Pass `--disable-blink-features=AutomationControlled` (default
    /// `true`; DECISIONS.md #12). Only the test that documents why the
    /// switch exists turns this off; the `browser:` block cannot.
    pub correct_automation_controlled: bool,
}

impl LaunchConfig {
    /// A headless, pipe-transport config for `path` with defaults
    /// (1280×800, no proxy, temp profile).
    pub fn for_path(path: impl Into<PathBuf>) -> LaunchConfig {
        LaunchConfig {
            path: path.into(),
            headless: true,
            size: (1280, 800),
            proxy: None,
            profile: None,
            flags: Vec::new(),
            env: Vec::new(),
            virtual_display: false,
            transport: TransportChoice::Pipe,
            correct_automation_controlled: true,
        }
    }

    /// The full argument vector for `user_data_dir`. **The** place flags
    /// are decided; see the module docs for the list.
    pub fn args(&self, user_data_dir: &Path) -> Vec<String> {
        self.args_with_pipe_flag(user_data_dir, PIPE_FLAG)
    }

    /// [`args`](Self::args) with the pipe flag spelled `pipe_flag` (Windows
    /// passes `--remote-debugging-io-pipes=<r>,<w>`).
    pub fn args_with_pipe_flag(&self, user_data_dir: &Path, pipe_flag: &str) -> Vec<String> {
        let mut a = Vec::with_capacity(8 + self.flags.len());
        match self.transport {
            TransportChoice::Pipe => a.push(pipe_flag.to_string()),
            TransportChoice::Port(p) => a.push(format!("--remote-debugging-port={p}")),
        }
        a.push(format!("--user-data-dir={}", user_data_dir.display()));
        a.push("--no-first-run".to_string());
        a.push("--no-default-browser-check".to_string());
        if self.correct_automation_controlled {
            // see DECISIONS.md #12 — Chrome treats a configured debugger
            // pipe/port as automation and sets navigator.webdriver = true
            // (https://issues.chromium.org/issues/40746300). This is the one
            // permitted --disable-* switch, on launched browsers only.
            a.push(AUTOMATION_CONTROLLED_OFF.to_string());
        }
        a.push(format!("--window-size={},{}", self.size.0, self.size.1));
        if self.headless {
            a.push("--headless".to_string());
        }
        if let Some(proxy) = &self.proxy {
            a.push(format!("--proxy-server={}", proxy.server_arg()));
        }
        a.extend(self.flags.iter().cloned());
        a.push("about:blank".to_string());
        a
    }
}

/// The `--user-data-dir`: a temp dir Surf owns (removed on close) or the
/// user's `profile:` path (kept).
#[derive(Debug)]
pub struct ProfileDir {
    path: PathBuf,
    is_temp: bool,
    temp: Mutex<Option<tempfile::TempDir>>,
}

impl ProfileDir {
    /// A fresh `surf-profile-*` temp dir.
    pub fn temp() -> io::Result<ProfileDir> {
        let t = tempfile::Builder::new().prefix("surf-profile-").tempdir()?;
        Ok(ProfileDir {
            path: t.path().to_path_buf(),
            is_temp: true,
            temp: Mutex::new(Some(t)),
        })
    }

    /// The user's `profile:` dir (created if missing, never removed).
    pub fn persistent(path: &Path) -> io::Result<ProfileDir> {
        std::fs::create_dir_all(path)?;
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        Ok(ProfileDir {
            path,
            is_temp: false,
            temp: Mutex::new(None),
        })
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether Surf owns (and will remove) it.
    pub fn is_temp(&self) -> bool {
        self.is_temp
    }

    /// Remove a temp dir, retrying for a moment: Chrome's helper processes
    /// may still be writing into it right after the browser exits. A
    /// persistent profile is left alone. Idempotent.
    pub async fn cleanup(&self) {
        if let Some(task) = self.detach_cleanup() {
            task.await;
        }
    }

    /// Take the temp dir out and return a future that removes it (with the
    /// retry loop of [`cleanup`](Self::cleanup)); `None` for a persistent
    /// profile or when already taken. The caller decides whether to await
    /// it or spawn it (`Launched::close` spawns so the shutdown ladder
    /// does not wait on the file system; `Launched::cleanup` joins it).
    fn detach_cleanup(&self) -> Option<impl Future<Output = ()> + Send + 'static> {
        let temp = self.temp.lock().expect("profile poisoned").take()?;
        let path = self.path.clone();
        Some(async move {
            for attempt in 0..20u32 {
                match std::fs::remove_dir_all(&path) {
                    Ok(()) => break,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => break,
                    Err(e) => {
                        if attempt == 19 {
                            tracing::warn!("could not remove temp profile {}: {e}", path.display());
                        } else {
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                    }
                }
            }
            // `TempDir::drop` makes one more best-effort attempt.
            drop(temp);
        })
    }

    /// Synchronous best-effort removal (used from `Drop`).
    fn cleanup_blocking(&self) {
        if let Ok(mut guard) = self.temp.lock() {
            if let Some(t) = guard.take() {
                let _ = t.close();
            }
        }
    }
}

/// The last [`STDERR_TAIL_LINES`] lines Chrome wrote to stderr.
#[derive(Debug, Clone, Default)]
pub struct StderrTail(Arc<Mutex<VecDeque<String>>>);

impl StderrTail {
    fn push(&self, line: String) {
        let mut q = self.0.lock().expect("stderr tail poisoned");
        if q.len() == STDERR_TAIL_LINES {
            q.pop_front();
        }
        q.push_back(line);
    }

    /// The buffered lines joined with `\n`.
    pub fn snapshot(&self) -> String {
        let q = self.0.lock().expect("stderr tail poisoned");
        q.iter().map(String::as_str).collect::<Vec<_>>().join("\n")
    }
}

/// How the browser process ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// Exit code, if it exited normally.
    pub code: Option<i32>,
    /// Terminating signal (unix), if killed.
    pub signal: Option<i32>,
}

impl Exit {
    fn from_status(status: std::process::ExitStatus) -> Exit {
        #[cfg(unix)]
        let signal = std::os::unix::process::ExitStatusExt::signal(&status);
        #[cfg(not(unix))]
        let signal = None;
        Exit {
            code: status.code(),
            signal,
        }
    }

    /// `exit code N` / `signal N` for messages.
    pub fn describe(&self) -> String {
        match (self.code, self.signal) {
            (Some(c), _) => format!("exit code {c}"),
            (None, Some(s)) => format!("signal {s}"),
            (None, None) => "unknown status".into(),
        }
    }
}

enum Control {
    Terminate,
    Kill,
}

/// Handle to the browser OS process: pid, exit status, and the kill
/// switches. The `Child` itself lives in a watcher task.
#[derive(Debug)]
pub struct Process {
    pid: u32,
    control: mpsc::UnboundedSender<Control>,
    exit: watch::Receiver<Option<Exit>>,
    expected_exit: Arc<AtomicBool>,
    stderr: StderrTail,
}

impl Process {
    /// Hand `child` to a watcher task. `conn_slot` is filled with the
    /// connection once it exists so an unexpected exit can mark it crashed.
    fn watch(mut child: Child, conn_slot: Arc<OnceLock<Weak<Connection>>>) -> Process {
        let pid = child.id().unwrap_or(0);
        let stderr = StderrTail::default();
        if let Some(err) = child.stderr.take() {
            tokio::spawn(drain_stderr(err, stderr.clone()));
        }
        let (control_tx, control_rx) = mpsc::unbounded_channel();
        let (exit_tx, exit_rx) = watch::channel(None);
        let expected_exit = Arc::new(AtomicBool::new(false));
        tokio::spawn(watch_process(
            child,
            control_rx,
            exit_tx,
            conn_slot,
            stderr.clone(),
            expected_exit.clone(),
        ));
        Process {
            pid,
            control: control_tx,
            exit: exit_rx,
            expected_exit,
            stderr,
        }
    }

    /// OS process id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Exit status if the process has ended.
    pub fn exit_status(&self) -> Option<Exit> {
        *self.exit.borrow()
    }

    /// Whether the process has ended.
    pub fn has_exited(&self) -> bool {
        self.exit_status().is_some()
    }

    /// Wait up to `timeout` for the process to end.
    pub async fn wait_exit(&self, timeout: Duration) -> Option<Exit> {
        let mut rx = self.exit.clone();
        let waited = tokio::time::timeout(timeout, rx.wait_for(|e| e.is_some())).await;
        match waited {
            Ok(Ok(v)) => *v,
            _ => self.exit_status(),
        }
    }

    /// Declare that an exit from now on is intended (shutdown), so the
    /// watcher does not report a crash.
    pub fn expect_exit(&self) {
        self.expected_exit.store(true, Ordering::SeqCst);
    }

    /// `SIGTERM` (unix) / `TerminateProcess` (windows).
    pub fn terminate(&self) {
        let _ = self.control.send(Control::Terminate);
    }

    /// `SIGKILL` / `TerminateProcess`.
    pub fn kill(&self) {
        let _ = self.control.send(Control::Kill);
    }

    /// The stderr ring buffer.
    pub fn stderr(&self) -> &StderrTail {
        &self.stderr
    }
}

async fn drain_stderr(stderr: ChildStderr, tail: StderrTail) {
    let mut lines = BufReader::new(stderr).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        tracing::trace!(target: "surf_browser::chrome", "{line}");
        tail.push(line);
    }
}

async fn watch_process(
    mut child: Child,
    mut control: mpsc::UnboundedReceiver<Control>,
    exit_tx: watch::Sender<Option<Exit>>,
    conn_slot: Arc<OnceLock<Weak<Connection>>>,
    stderr: StderrTail,
    expected_exit: Arc<AtomicBool>,
) {
    let pid = child.id();
    let mut control_open = true;
    let status = loop {
        tokio::select! {
            r = child.wait() => break r,
            c = control.recv(), if control_open => match c {
                Some(Control::Terminate) => match pid {
                    Some(pid) => sys::terminate(pid),
                    None => { let _ = child.start_kill(); }
                },
                Some(Control::Kill) => { let _ = child.start_kill(); }
                None => control_open = false,
            },
        }
    };
    let exit = match status {
        Ok(s) => Exit::from_status(s),
        Err(e) => {
            tracing::warn!("waiting for the browser process failed: {e}");
            Exit {
                code: None,
                signal: None,
            }
        }
    };
    if expected_exit.load(Ordering::SeqCst) {
        tracing::debug!("browser process {pid:?} exited ({})", exit.describe());
    } else {
        let mut tail = stderr.snapshot();
        if let Some(sig) = exit.signal {
            tail = format!("killed by signal {sig}\n{tail}");
        }
        tracing::error!(
            "browser process {pid:?} exited unexpectedly ({})",
            exit.describe()
        );
        if let Some(conn) = conn_slot.get().and_then(Weak::upgrade) {
            conn.mark_crashed(exit.code, tail);
        }
    }
    // After `mark_crashed`, so a driver waiting on this sees the crash.
    exit_tx.send_replace(Some(exit));
}

/// Wraps the transport so that when the pipe/socket closes the driver
/// waits briefly for the process watcher — in-flight calls then fail with
/// `BrowserCrashed` (with the stderr tail) rather than a bare `Closed`.
struct ProcessAwareTransport {
    inner: Box<dyn Transport>,
    exit: watch::Receiver<Option<Exit>>,
}

impl Transport for ProcessAwareTransport {
    fn send(&mut self, frame: &[u8]) -> BoxFuture<'_, io::Result<()>> {
        self.inner.send(frame)
    }

    fn recv(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>> {
        Box::pin(async move {
            let r = self.inner.recv().await;
            if !matches!(r, Ok(Some(_))) {
                let _ = tokio::time::timeout(EXIT_GRACE, self.exit.wait_for(|e| e.is_some())).await;
            }
            r
        })
    }
}

/// A running browser: the connection, the process, the profile, the
/// optional virtual display, and the proxy credentials for the
/// `Fetch.authRequired` handler. [`close`](Launched::close) is the only
/// way to end it cleanly; dropping it kills the process best-effort.
pub struct Launched {
    /// The CDP connection (pipe or websocket).
    pub connection: Arc<Connection>,
    /// The OS process.
    pub process: Process,
    /// The `--user-data-dir`.
    pub profile_dir: ProfileDir,
    /// The Xvfb server, if Surf started one.
    pub display: Option<VirtualDisplay>,
    /// `user:pass` from the proxy URL — never on the command line; page.rs
    /// answers `Fetch.authRequired` with them.
    pub proxy_credentials: Option<Credentials>,
    /// `Browser.getVersion.product`, e.g. `Chrome/131.0.6778.86`.
    pub product: String,
    /// The binary that was launched.
    pub path: PathBuf,
    /// The argument vector that was passed (for `surf doctor` / tracing).
    pub args: Vec<String>,
    closed: AtomicBool,
    /// The temp-profile removal spawned by [`close`](Self::close); joined
    /// by [`cleanup`](Self::cleanup).
    cleanup: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl std::fmt::Debug for Launched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Launched")
            .field("pid", &self.process.pid())
            .field("product", &self.product)
            .field("profile", &self.profile_dir.path())
            .field("closed", &self.closed.load(Ordering::Relaxed))
            .finish()
    }
}

impl Launched {
    /// The browser-level session.
    pub fn root(&self) -> Session {
        self.connection.root()
    }

    /// The last lines of Chrome's stderr.
    pub fn stderr_tail(&self) -> String {
        self.process.stderr().snapshot()
    }

    /// OS process id.
    pub fn pid(&self) -> u32 {
        self.process.pid()
    }

    /// Whether [`close`](Self::close) has run.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// The shutdown ladder: `Browser.close` → wait 2 s → `SIGTERM` → wait
    /// 2 s → `SIGKILL`; then close the connection, remove a temp profile
    /// (a `profile:` dir is never touched), stop Xvfb. Idempotent; safe to
    /// call from a Ctrl-C handler.
    pub async fn close(&self) {
        self.close_inner(false).await;
        self.cleanup().await;
    }

    /// [`close`](Self::close) with the temp-profile removal (which waits
    /// for Chrome's helpers to let go of the directory) on a background
    /// task instead of inline; [`cleanup`](Self::cleanup) joins it. The
    /// caller must do so before the process exits — `std::process::exit`
    /// runs no destructors, so an unjoined task leaks the directory.
    pub async fn close_detached(&self) {
        self.close_inner(true).await;
    }

    async fn close_inner(&self, detach: bool) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.process.expect_exit();
        if !self.process.has_exited() {
            if !self.connection.is_closed() {
                let root = self.connection.root().with_timeout(Some(CLOSE_GRACE));
                let _ = root.call_raw("Browser.close", serde_json::json!({})).await;
            }
            if self.process.wait_exit(CLOSE_GRACE).await.is_none() {
                tracing::warn!(
                    "browser {} did not exit after Browser.close; sending SIGTERM",
                    self.pid()
                );
                self.process.terminate();
                if self.process.wait_exit(CLOSE_GRACE).await.is_none() {
                    tracing::warn!("browser {} ignored SIGTERM; sending SIGKILL", self.pid());
                    self.process.kill();
                    let _ = self.process.wait_exit(KILL_WAIT).await;
                }
            }
        }
        self.connection.close();
        if let Some(remove) = self.profile_dir.detach_cleanup() {
            match tokio::runtime::Handle::try_current() {
                Ok(h) if detach => {
                    *self.cleanup.lock().expect("cleanup") = Some(h.spawn(remove));
                }
                _ => remove.await,
            }
        }
        if let Some(d) = &self.display {
            d.stop().await;
        }
        tracing::debug!("browser {} closed", self.pid());
    }

    /// Join the temp-profile removal started by
    /// [`close_detached`](Self::close_detached); remove inline when nothing
    /// started it. No-op for a `profile:` dir or after [`close`](Self::close).
    /// Idempotent.
    pub async fn cleanup(&self) {
        let task = self.cleanup.lock().expect("cleanup").take();
        if let Some(t) = task {
            let _ = t.await;
        }
        // `close` not called (or no runtime): remove inline.
        self.profile_dir.cleanup().await;
    }
}

impl Drop for Launched {
    fn drop(&mut self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.process.expect_exit();
        self.process.kill();
        self.connection.close();
        self.profile_dir.cleanup_blocking();
    }
}

/// Launch Chrome per `cfg`. See the module docs.
pub async fn launch(cfg: LaunchConfig) -> Result<Launched, BrowserError> {
    let profile_dir = match &cfg.profile {
        Some(p) => ProfileDir::persistent(p)
            .map_err(|e| BrowserError::Launch(format!("profile {}: {e}", p.display())))?,
        None => ProfileDir::temp()
            .map_err(|e| BrowserError::Launch(format!("could not create temp profile: {e}")))?,
    };
    let display = match VirtualDisplay::start_if_needed(&cfg).await {
        Ok(d) => d,
        Err(e) => {
            profile_dir.cleanup().await;
            return Err(e);
        }
    };

    // Pipe pair first so the child ends exist at spawn time.
    let (pipe_transport, child_fds) = match cfg.transport {
        TransportChoice::Pipe => {
            let (t, fds) = pipe::create_pair()
                .map_err(|e| BrowserError::Launch(format!("could not create pipes: {e}")))?;
            (Some(t), Some(fds))
        }
        TransportChoice::Port(_) => (None, None),
    };

    #[cfg(windows)]
    let pipe_flag = child_fds
        .as_ref()
        .map(|f| f.io_pipes_arg())
        .unwrap_or_else(|| PIPE_FLAG.to_string());
    #[cfg(not(windows))]
    let pipe_flag = PIPE_FLAG.to_string();
    let args = cfg.args_with_pipe_flag(profile_dir.path(), &pipe_flag);

    let mut cmd = Command::new(&cfg.path);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in &cfg.env {
        cmd.env(k, v);
    }
    if let Some(d) = &display {
        cmd.env("DISPLAY", d.display());
    }
    #[cfg(unix)]
    if let Some(fds) = &child_fds {
        let (r, w) = fds.raw();
        sys::install_pipe_fds(&mut cmd, r, w);
    }
    tracing::debug!("launching {} {}", cfg.path.display(), args.join(" "));

    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            drop(child_fds);
            profile_dir.cleanup().await;
            if let Some(d) = &display {
                d.stop().await;
            }
            return Err(BrowserError::Launch(format!(
                "could not start {}: {e}",
                cfg.path.display()
            )));
        }
    };
    // The parent must not keep the child's pipe ends open, or it would
    // never see EOF when Chrome exits.
    drop(child_fds);

    let conn_slot: Arc<OnceLock<Weak<Connection>>> = Arc::new(OnceLock::new());
    let process = Process::watch(child, conn_slot.clone());

    let transport: Box<dyn Transport> = match (cfg.transport, pipe_transport) {
        (TransportChoice::Pipe, Some(t)) => Box::new(t),
        (TransportChoice::Port(port), _) => {
            match connect_port(port, profile_dir.path(), &process).await {
                Ok(t) => t,
                Err(e) => {
                    abort_launch(&process, &profile_dir, display.as_ref()).await;
                    return Err(e);
                }
            }
        }
        (TransportChoice::Pipe, None) => unreachable!("pipe transport created above"),
    };
    let transport = ProcessAwareTransport {
        inner: transport,
        exit: process.exit.clone(),
    };
    let connection = Connection::new(Box::new(transport));
    let _ = conn_slot.set(Arc::downgrade(&connection));

    let root = connection.root().with_timeout(Some(STARTUP_TIMEOUT));
    let version = tokio::time::timeout(STARTUP_TIMEOUT, root.send(browser::GetVersion {})).await;
    let product = match version {
        Ok(Ok(v)) => v.product,
        Ok(Err(e)) => {
            let tail = process.stderr().snapshot();
            let msg = match e {
                surf_cdp::CdpError::BrowserCrashed { .. } => {
                    format!("{} exited during startup: {e}", cfg.path.display())
                }
                _ => format!(
                    "{} did not answer Browser.getVersion: {e}\nstderr:\n{tail}",
                    cfg.path.display()
                ),
            };
            connection.close();
            abort_launch(&process, &profile_dir, display.as_ref()).await;
            return Err(BrowserError::Launch(msg));
        }
        Err(_) => {
            let tail = process.stderr().snapshot();
            connection.close();
            abort_launch(&process, &profile_dir, display.as_ref()).await;
            return Err(BrowserError::Launch(format!(
                "{} did not answer Browser.getVersion within {STARTUP_TIMEOUT:?}\nstderr:\n{tail}",
                cfg.path.display()
            )));
        }
    };
    tracing::info!(
        "launched {product} (pid {}, {}, profile {})",
        process.pid(),
        if cfg.headless { "headless" } else { "headed" },
        profile_dir.path().display()
    );
    Ok(Launched {
        connection,
        process,
        profile_dir,
        display,
        proxy_credentials: cfg.proxy.as_ref().and_then(|p| p.credentials.clone()),
        product,
        path: cfg.path,
        args,
        closed: AtomicBool::new(false),
        cleanup: Mutex::new(None),
    })
}

/// Stop a half-launched browser and clean up (startup failures): `SIGTERM`,
/// then `SIGKILL`, remove a temp profile, stop Xvfb.
async fn abort_launch(
    process: &Process,
    profile_dir: &ProfileDir,
    display: Option<&VirtualDisplay>,
) {
    process.expect_exit();
    if !process.has_exited() {
        process.terminate();
        if process.wait_exit(CLOSE_GRACE).await.is_none() {
            process.kill();
            let _ = process.wait_exit(KILL_WAIT).await;
        }
    }
    profile_dir.cleanup().await;
    if let Some(d) = display {
        d.stop().await;
    }
}

/// `cdp: N`: wait for Chrome to listen (or, for port 0, to write
/// `DevToolsActivePort`), then connect via `/json/version`.
async fn connect_port(
    port: u16,
    profile_dir: &Path,
    process: &Process,
) -> Result<Box<dyn Transport>, BrowserError> {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    let mut last_err: Option<io::Error> = None;
    loop {
        let actual = if port == 0 {
            std::fs::read_to_string(profile_dir.join("DevToolsActivePort"))
                .ok()
                .and_then(|s| s.lines().next()?.trim().parse::<u16>().ok())
        } else {
            Some(port)
        };
        if let Some(p) = actual {
            match surf_cdp::transport::tcp::connect_with_timeout(
                "127.0.0.1",
                p,
                PORT_CONNECT_ATTEMPT,
            )
            .await
            {
                Ok(t) => return Ok(Box::new(t)),
                Err(e) => last_err = Some(e),
            }
        }
        if let Some(exit) = process.exit_status() {
            return Err(BrowserError::Launch(format!(
                "browser exited during startup ({})\nstderr:\n{}",
                exit.describe(),
                process.stderr().snapshot()
            )));
        }
        if Instant::now() >= deadline {
            return Err(BrowserError::Launch(format!(
                "browser did not open the DevTools port within {STARTUP_TIMEOUT:?} ({})\nstderr:\n{}",
                last_err.map(|e| e.to_string()).unwrap_or_else(|| "no port yet".into()),
                process.stderr().snapshot()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
mod sys {
    //! The one place `unsafe` is needed: installing fds between `fork` and
    //! `exec`, and sending `SIGTERM`.

    use std::io;
    use std::os::fd::RawFd;
    use tokio::process::Command;

    /// Put the child's pipe ends on fd 3 (Chrome reads) and fd 4 (Chrome
    /// writes). `dup2` clears `CLOEXEC` on the new descriptor only, so the
    /// originals close at `exec`.
    pub(super) fn install_pipe_fds(cmd: &mut Command, read: RawFd, write: RawFd) {
        // SAFETY: the closure runs between fork and exec and calls only
        // async-signal-safe functions (`fcntl`, `dup2`); it allocates
        // nothing and takes no locks.
        unsafe {
            cmd.pre_exec(move || {
                let read = move_off_3_4(read)?;
                let write = move_off_3_4(write)?;
                if libc::dup2(read, 3) < 0 || libc::dup2(write, 4) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    /// If `fd` happens to be 3 or 4 already, duplicate it to a descriptor
    /// ≥ 5 (`CLOEXEC`) so the `dup2` calls cannot clobber it.
    fn move_off_3_4(fd: RawFd) -> io::Result<RawFd> {
        if fd != 3 && fd != 4 {
            return Ok(fd);
        }
        // SAFETY: `fcntl` is async-signal-safe; `fd` is open.
        let n = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 5) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n)
    }

    /// `SIGTERM` the process (Chrome shuts down cleanly on it).
    pub(super) fn terminate(pid: u32) {
        // SAFETY: plain syscall; a stale pid is harmless because the child
        // is still owned by the watcher task (not yet reaped).
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}

#[cfg(windows)]
mod sys {
    /// No `SIGTERM` on Windows: the watcher falls back to `TerminateProcess`.
    pub(super) fn terminate(_pid: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts_with(proxy: Option<&str>, flags: &[&str]) -> LaunchConfig {
        LaunchOptions {
            proxy: proxy.map(str::to_string),
            flags: flags.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
        .resolve_with(PathBuf::from("/opt/chrome"), false)
        .unwrap()
    }

    #[test]
    fn flags_are_quiet_and_in_order() {
        let cfg = opts_with(Some("http://u:p@proxy:8080"), &["--lang=en-US"]);
        let args = cfg.args(Path::new("/tmp/x"));
        assert_eq!(
            args,
            vec![
                "--remote-debugging-pipe",
                "--user-data-dir=/tmp/x",
                "--no-first-run",
                "--no-default-browser-check",
                "--disable-blink-features=AutomationControlled",
                "--window-size=1280,800",
                "--proxy-server=http://proxy:8080",
                "--lang=en-US",
                "about:blank",
            ]
        );
        for a in &args {
            assert!(!a.contains("enable-automation"), "{a}");
            // DECISIONS.md #12: exactly one --disable-* switch, and no other.
            assert!(
                !a.starts_with("--disable-") || a == AUTOMATION_CONTROLLED_OFF,
                "{a}"
            );
            assert!(!a.contains("u:p@"), "credentials leaked: {a}");
        }
        assert_eq!(
            args.iter().filter(|a| a.starts_with("--disable-")).count(),
            1
        );
        let without = LaunchConfig {
            correct_automation_controlled: false,
            ..cfg.clone()
        };
        assert!(!without
            .args(Path::new("/tmp/x"))
            .iter()
            .any(|a| a.starts_with("--disable-")));
        assert_eq!(
            cfg.proxy.unwrap().credentials,
            Some(Credentials {
                username: "u".into(),
                password: "p".into()
            })
        );
    }

    #[test]
    fn headless_flag_only_when_headless() {
        let on = LaunchOptions::default()
            .resolve_with("/opt/chrome".into(), true)
            .unwrap();
        assert!(on
            .args(Path::new("/tmp/x"))
            .contains(&"--headless".to_string()));
        let off = LaunchOptions::default()
            .resolve_with("/opt/chrome".into(), false)
            .unwrap();
        assert!(!off
            .args(Path::new("/tmp/x"))
            .contains(&"--headless".to_string()));
    }

    #[test]
    fn port_mode_and_user_disable_flags_pass_through() {
        let cfg = LaunchOptions {
            cdp: CdpMode::Port(9222),
            flags: vec!["--disable-gpu".into()],
            ..Default::default()
        }
        .resolve_with("/opt/chrome".into(), true)
        .unwrap();
        let args = cfg.args(Path::new("/tmp/x"));
        assert_eq!(args[0], "--remote-debugging-port=9222");
        assert!(!args.iter().any(|a| a == "--remote-debugging-pipe"));
        // The user asked for it: passed verbatim, after Surf's own flags.
        let i = args.iter().position(|a| a == "--disable-gpu").unwrap();
        assert!(i > args.iter().position(|a| a == "--headless").unwrap());
        assert_eq!(args.last().unwrap(), "about:blank");
    }

    #[test]
    fn attach_and_apostate_do_not_launch() {
        let e = LaunchOptions {
            cdp: CdpMode::Attach("ws://x".into()),
            ..Default::default()
        }
        .resolve_with("/opt/chrome".into(), true)
        .unwrap_err();
        assert!(matches!(e, BrowserError::Config { .. }), "{e}");
        let e = LaunchOptions {
            engine: "apostate".into(),
            ..Default::default()
        }
        .resolve()
        .unwrap_err();
        assert!(e.to_string().contains("not available yet"), "{e}");
    }

    #[test]
    fn proxy_spec_parses_all_shapes() {
        let p = ProxySpec::parse("http://user:p%40ss@host:8080").unwrap();
        assert_eq!(p.scheme, "http");
        assert_eq!(p.host, "host");
        assert_eq!(p.port, Some(8080));
        assert_eq!(
            p.credentials,
            Some(Credentials {
                username: "user".into(),
                password: "p@ss".into()
            })
        );
        assert_eq!(p.server_arg(), "http://host:8080");

        let p = ProxySpec::parse("socks5://127.0.0.1:1080").unwrap();
        assert_eq!(p.scheme, "socks5");
        assert_eq!(p.credentials, None);
        assert_eq!(p.server_arg(), "socks5://127.0.0.1:1080");

        let p = ProxySpec::parse("proxy.example:3128").unwrap();
        assert_eq!(p.scheme, "http");
        assert_eq!(p.server_arg(), "http://proxy.example:3128");

        let p = ProxySpec::parse("https://[::1]:8443").unwrap();
        assert_eq!(p.host, "[::1]");
        assert_eq!(p.port, Some(8443));

        let p = ProxySpec::parse("http://only@host").unwrap();
        assert_eq!(p.port, None);
        assert_eq!(p.credentials.unwrap().password, "");

        for bad in ["", "http://", "http://host:notaport", "http://[::1"] {
            assert!(ProxySpec::parse(bad).is_err(), "{bad:?} should fail");
        }
    }

    #[test]
    fn stderr_tail_is_bounded() {
        let t = StderrTail::default();
        for i in 0..(STDERR_TAIL_LINES + 10) {
            t.push(format!("line {i}"));
        }
        let s = t.snapshot();
        assert!(!s.contains("line 9\n"), "{s}");
        assert!(s.starts_with("line 10\n"), "{s}");
        assert!(s.ends_with(&format!("line {}", STDERR_TAIL_LINES + 9)));
    }

    #[test]
    fn profile_dir_temp_is_removed_persistent_is_kept() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let t = ProfileDir::temp().unwrap();
            let p = t.path().to_path_buf();
            assert!(p.is_dir() && t.is_temp());
            std::fs::write(p.join("junk"), b"x").unwrap();
            t.cleanup().await;
            assert!(!p.exists());
            t.cleanup().await; // idempotent

            let root = tempfile::tempdir().unwrap();
            let user = root.path().join("profiles/alice");
            let d = ProfileDir::persistent(&user).unwrap();
            assert!(user.is_dir() && !d.is_temp());
            d.cleanup().await;
            assert!(user.is_dir(), "persistent profile must survive cleanup");
        });
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("x%4"), "x%4");
        assert_eq!(percent_decode("%41%42"), "AB");
    }
}
