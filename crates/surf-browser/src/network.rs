//! Request / response hooks, blocking, interception, proxy auth, downloads.
//!
//! Nothing here is on by default. `Network` and `Fetch` are held by
//! ref-counted [`DomainGuard`]s owned by whoever needs them and are
//! disabled again when the last owner goes away:
//!
//! | need | domain | owner |
//! |------|--------|-------|
//! | `on request` / `on response` hooks | `Network` | [`NetworkHooks`] |
//! | `block([...])` | `Network` (`Network.setBlockedURLs` needs it) | the page, while the list is non-empty |
//! | `intercept(pattern):` | `Fetch` | [`Interception`] |
//! | proxy credentials | `Fetch` (`handleAuthRequests`) | the page, until the first challenge is answered |
//!
//! `Fetch` has one owner per page session, the [`FetchHub`]: interception
//! and proxy authentication both go through it, so there is exactly one
//! `Fetch.enable` whose `handleAuthRequests` / `patterns` are recomputed
//! whenever either need changes (a hook can no longer switch auth handling
//! off by sending its own `enable`). The hub answers `Fetch.authRequired`
//! with the context's credentials when `authChallenge.source == "Proxy"`
//! (`Default` otherwise, so a site's own 401 still reaches the page) and
//! forwards `Fetch.requestPaused` to the interceptor when its URL matches,
//! continuing everything else untouched. Chrome caches proxy credentials
//! per context, so after the first answered challenge the auth need is
//! dropped and `Fetch` goes away unless an interceptor still holds it.
//!
//! Credentials never go on the command line (`--proxy-server` carries only
//! `scheme://host:port`).
//!
//! Downloads: `downloads: dir` makes the browser call
//! `Browser.setDownloadBehavior{behavior: allowAndName, downloadPath,
//! eventsEnabled: true}` (per browser context) and run one
//! [`DownloadTracker`] on the root session that follows
//! `Browser.downloadWillBegin` / `downloadProgress`, renames the finished
//! `<guid>` file to its suggested name and serves `wait_download()`.

use crate::error::BrowserError;
use crate::launch::Credentials;
use crate::util::{base64_decode, base64_encode};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use surf_cdp::protocol::{fetch, network};
use surf_cdp::{event, CdpError, DomainGuard, Event, Session};
use tokio::sync::{broadcast, mpsc, Notify};
use tokio::task::JoinHandle;

// ───────────────────────── URL patterns ─────────────────────────

/// A URL pattern for hooks, interception and `wait_url`: a glob where `*`
/// (and `**`, accepted as a synonym) matches any run of characters
/// including `/`, anchored at both ends — or a regular expression after a
/// `re:` prefix. An empty pattern matches everything.
#[derive(Debug, Clone)]
pub struct UrlPattern {
    source: String,
    kind: PatternKind,
}

#[derive(Debug, Clone)]
enum PatternKind {
    Glob(Vec<String>),
    Regex(regex::Regex),
}

impl PartialEq for UrlPattern {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
    }
}

impl Eq for UrlPattern {}

impl UrlPattern {
    /// Parse `*` globs or `re:…`; a bad regex is a [`BrowserError::Config`].
    pub fn parse(pattern: &str) -> Result<UrlPattern, BrowserError> {
        let kind = match pattern.strip_prefix("re:") {
            Some(re) => {
                PatternKind::Regex(regex::Regex::new(re).map_err(|e| BrowserError::Config {
                    what: "URL pattern".into(),
                    reason: format!("{pattern:?}: {e}"),
                })?)
            }
            None => PatternKind::Glob(
                pattern
                    .replace("**", "*")
                    .split('*')
                    .map(str::to_owned)
                    .collect(),
            ),
        };
        Ok(UrlPattern {
            source: pattern.to_owned(),
            kind,
        })
    }

    /// A glob pattern (never fails).
    pub fn glob(pattern: &str) -> UrlPattern {
        UrlPattern::parse(if pattern.starts_with("re:") {
            "*"
        } else {
            pattern
        })
        .expect("glob patterns always parse")
    }

    /// The pattern as written.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether `url` matches.
    pub fn matches(&self, url: &str) -> bool {
        match &self.kind {
            PatternKind::Regex(re) => re.is_match(url),
            PatternKind::Glob(parts) => glob_match(parts, url),
        }
    }

    /// The pattern in the dialect `Network.setBlockedURLs` / `Fetch.enable`
    /// understand (`*` wildcards only; a regex becomes `*`).
    pub fn chrome_pattern(&self) -> String {
        match &self.kind {
            PatternKind::Regex(_) => "*".into(),
            PatternKind::Glob(parts) => parts.join("*"),
        }
    }
}

/// Anchored glob match: `parts` are the literal pieces between `*`s.
fn glob_match(parts: &[String], s: &str) -> bool {
    let Some((first, rest_parts)) = parts.split_first() else {
        return s.is_empty();
    };
    if !s.starts_with(first.as_str()) {
        return false;
    }
    if rest_parts.is_empty() {
        return s.len() == first.len();
    }
    let mut rest = &s[first.len()..];
    let last = rest_parts.len() - 1;
    for (i, p) in rest_parts.iter().enumerate() {
        if i == last {
            return rest.ends_with(p.as_str());
        }
        match rest.find(p.as_str()) {
            Some(at) => rest = &rest[at + p.len()..],
            None => return false,
        }
    }
    true
}

// ───────────────────────── headers ─────────────────────────

/// Header map from a CDP `Headers` object, keys lower-cased.
pub fn headers_of(v: &Value) -> Vec<(String, String)> {
    v.as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| {
                    (
                        k.to_ascii_lowercase(),
                        v.as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| v.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

// ───────────────────────── hooks (Network) ─────────────────────────

/// What `on request` sees: `Network.requestWillBeSent`.
#[derive(Debug, Clone)]
pub struct RequestInfo {
    /// `Network.RequestId` (also keys the matching response).
    pub request_id: String,
    /// URL.
    pub url: String,
    /// Method.
    pub method: String,
    /// Request headers, keys lower-cased.
    pub headers: Vec<(String, String)>,
    /// `Document`, `XHR`, `Fetch`, `Image`, `Script`, `Stylesheet`, …
    pub resource_type: String,
    /// POST body when Chrome included it.
    pub post_data: Option<String>,
    /// Frame that issued the request.
    pub frame_id: Option<String>,
}

impl RequestInfo {
    /// From the event's params.
    pub fn from_event(p: &Value) -> RequestInfo {
        let r = &p["request"];
        RequestInfo {
            request_id: p["requestId"].as_str().unwrap_or("").to_owned(),
            url: r["url"].as_str().unwrap_or("").to_owned(),
            method: r["method"].as_str().unwrap_or("").to_owned(),
            headers: headers_of(&r["headers"]),
            resource_type: p["type"].as_str().unwrap_or("Other").to_owned(),
            post_data: r["postData"].as_str().map(str::to_owned),
            frame_id: p["frameId"].as_str().map(str::to_owned),
        }
    }
}

/// What `on response` sees: `Network.responseReceived`.
#[derive(Debug, Clone)]
pub struct ResponseInfo {
    /// `Network.RequestId`.
    pub request_id: String,
    /// URL.
    pub url: String,
    /// HTTP status.
    pub status: i64,
    /// Status text.
    pub status_text: String,
    /// Response headers, keys lower-cased.
    pub headers: Vec<(String, String)>,
    /// MIME type.
    pub mime_type: String,
    /// Resource type.
    pub resource_type: String,
    /// Served from cache / service worker.
    pub from_cache: bool,
    /// Frame.
    pub frame_id: Option<String>,
}

impl ResponseInfo {
    /// From the event's params.
    pub fn from_event(p: &Value) -> ResponseInfo {
        let r = &p["response"];
        ResponseInfo {
            request_id: p["requestId"].as_str().unwrap_or("").to_owned(),
            url: r["url"].as_str().unwrap_or("").to_owned(),
            status: r["status"].as_i64().unwrap_or(0),
            status_text: r["statusText"].as_str().unwrap_or("").to_owned(),
            headers: headers_of(&r["headers"]),
            mime_type: r["mimeType"].as_str().unwrap_or("").to_owned(),
            resource_type: p["type"].as_str().unwrap_or("Other").to_owned(),
            from_cache: r["fromDiskCache"].as_bool().unwrap_or(false)
                || r["fromServiceWorker"].as_bool().unwrap_or(false),
            frame_id: p["frameId"].as_str().map(str::to_owned),
        }
    }
}

/// A response body (`Network.getResponseBody`).
#[derive(Debug, Clone)]
pub struct ResponseBody {
    /// Raw bytes (base64 decoded when Chrome encoded them).
    pub bytes: Vec<u8>,
    /// Whether Chrome sent the body base64-encoded (binary content).
    pub binary: bool,
}

impl ResponseBody {
    /// The body as text (lossy for binary content).
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

/// Holds `Network` enabled on a page session and the event streams the
/// hooks consume. Subscribed before `Network.enable`, so nothing is
/// missed. Dropping it releases the domain.
pub struct NetworkHooks {
    session: Session,
    _guard: DomainGuard,
    /// `Network.requestWillBeSent`.
    pub requests: broadcast::Receiver<Event>,
    /// `Network.responseReceived`.
    pub responses: broadcast::Receiver<Event>,
    /// `Network.loadingFinished` (the body can be fetched).
    pub finished: broadcast::Receiver<Event>,
    /// `Network.loadingFailed`.
    pub failed: broadcast::Receiver<Event>,
}

impl std::fmt::Debug for NetworkHooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkHooks").finish()
    }
}

impl NetworkHooks {
    /// Subscribe and enable `Network` on `session`.
    pub async fn install(session: Session) -> Result<NetworkHooks, BrowserError> {
        let requests = session.events("Network.requestWillBeSent");
        let responses = session.events("Network.responseReceived");
        let finished = session.events("Network.loadingFinished");
        let failed = session.events("Network.loadingFailed");
        let guard = session.enable_domain("Network").await?;
        Ok(NetworkHooks {
            session,
            _guard: guard,
            requests,
            responses,
            finished,
            failed,
        })
    }

    /// The session the hooks are installed on.
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// `Network.getResponseBody` for `request_id`. Call it after the
    /// matching `loadingFinished`; before that Chrome has no body yet.
    pub async fn response_body(&self, request_id: &str) -> Result<ResponseBody, BrowserError> {
        response_body(&self.session, request_id).await
    }

    /// Split into the part that keeps `Network` enabled and the event
    /// streams (so a consumer can hold them in separate places).
    pub fn into_parts(self) -> (NetworkHold, HookStreams) {
        (
            NetworkHold {
                session: self.session,
                _guard: self._guard,
            },
            HookStreams {
                requests: self.requests,
                responses: self.responses,
                finished: self.finished,
                failed: self.failed,
            },
        )
    }
}

/// Keeps `Network` enabled; answers `response_body`.
pub struct NetworkHold {
    session: Session,
    _guard: DomainGuard,
}

impl std::fmt::Debug for NetworkHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetworkHold").finish()
    }
}

impl NetworkHold {
    /// `Network.getResponseBody` for `request_id`.
    pub async fn response_body(&self, request_id: &str) -> Result<ResponseBody, BrowserError> {
        response_body(&self.session, request_id).await
    }
}

/// The event streams of [`NetworkHooks`].
pub struct HookStreams {
    /// `Network.requestWillBeSent`.
    pub requests: broadcast::Receiver<Event>,
    /// `Network.responseReceived`.
    pub responses: broadcast::Receiver<Event>,
    /// `Network.loadingFinished`.
    pub finished: broadcast::Receiver<Event>,
    /// `Network.loadingFailed`.
    pub failed: broadcast::Receiver<Event>,
}

/// `Network.getResponseBody` on `session`.
pub async fn response_body(
    session: &Session,
    request_id: &str,
) -> Result<ResponseBody, BrowserError> {
    let r = session
        .send(network::GetResponseBody {
            request_id: request_id.to_owned(),
        })
        .await?;
    Ok(if r.base64_encoded {
        ResponseBody {
            bytes: base64_decode(&r.body).map_err(BrowserError::Unsupported)?,
            binary: true,
        }
    } else {
        ResponseBody {
            bytes: r.body.into_bytes(),
            binary: false,
        }
    })
}

// ───────────────────────── Fetch hub ─────────────────────────

/// Who needs `Fetch` on a page session right now.
#[derive(Default)]
struct FetchState {
    /// Proxy credentials still to be supplied (cleared after the first
    /// answered challenge — Chrome caches them for the context).
    credentials: Option<Credentials>,
    /// The interceptor, if any.
    intercept: Option<Interceptor>,
    /// Held while anything needs the domain.
    guard: Option<DomainGuard>,
}

struct Interceptor {
    patterns: Vec<UrlPattern>,
    tx: mpsc::UnboundedSender<Event>,
}

/// Shared between the hub task, the page and interceptions.
pub struct FetchShared {
    session: Session,
    state: tokio::sync::Mutex<FetchState>,
}

impl FetchShared {
    /// Send (or re-send) `Fetch.enable` for the current needs, or release
    /// the domain when nothing needs it.
    async fn reconfigure(&self) -> Result<(), CdpError> {
        let mut st = self.state.lock().await;
        let need_auth = st.credentials.is_some();
        let need_intercept = st.intercept.is_some();
        if !need_auth && !need_intercept {
            if st.guard.take().is_some() {
                tracing::debug!("Fetch released");
            }
            return Ok(());
        }
        if st.guard.is_none() {
            st.guard = Some(self.session.enable_domain("Fetch").await?);
        }
        // `enable_domain` sends a bare `Fetch.enable`; re-send with the
        // flags (idempotent: the last `enable` wins).
        self.session
            .send(fetch::Enable {
                patterns: Some(vec![fetch::RequestPattern {
                    url_pattern: Some("*".into()),
                    resource_type: None,
                    request_stage: Some(fetch::RequestStage::Request),
                }]),
                handle_auth_requests: Some(need_auth),
            })
            .await?;
        tracing::debug!("Fetch enabled (auth: {need_auth}, intercept: {need_intercept})");
        Ok(())
    }
}

/// The single owner of `Fetch` on a page session; see the module docs.
/// Dropping it aborts the task and releases the domain.
pub struct FetchHub {
    shared: Arc<FetchShared>,
    task: JoinHandle<()>,
}

impl std::fmt::Debug for FetchHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetchHub").finish()
    }
}

impl FetchHub {
    /// Start the hub on `session`; with `credentials`, `Fetch` is enabled
    /// right away with `handleAuthRequests`.
    pub async fn install(
        session: Session,
        credentials: Option<Credentials>,
    ) -> Result<FetchHub, BrowserError> {
        // Subscribe before enabling so no event is missed.
        let paused = session.events("Fetch.requestPaused");
        let auth = session.events("Fetch.authRequired");
        let shared = Arc::new(FetchShared {
            session: session.clone(),
            state: tokio::sync::Mutex::new(FetchState {
                credentials,
                ..Default::default()
            }),
        });
        shared.reconfigure().await?;
        let task = tokio::spawn(run_hub(shared.clone(), paused, auth));
        Ok(FetchHub { shared, task })
    }

    /// Route `Fetch.requestPaused` events whose URL matches one of
    /// `patterns` to the returned [`Interception`] (replacing any earlier
    /// one on this session).
    pub async fn intercept(&self, patterns: Vec<UrlPattern>) -> Result<Interception, BrowserError> {
        let (tx, rx) = mpsc::unbounded_channel();
        {
            let mut st = self.shared.state.lock().await;
            st.intercept = Some(Interceptor { patterns, tx });
        }
        self.shared.reconfigure().await?;
        Ok(Interception {
            rx,
            shared: self.shared.clone(),
        })
    }
}

impl Drop for FetchHub {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run_hub(
    shared: Arc<FetchShared>,
    mut paused: broadcast::Receiver<Event>,
    mut auth: broadcast::Receiver<Event>,
) {
    loop {
        tokio::select! {
            ev = event::next(&mut paused) => {
                let Some(ev) = ev else { break };
                let Some(id) = ev.params["requestId"].as_str() else { continue };
                let url = ev.params["request"]["url"].as_str().unwrap_or("");
                let forwarded = {
                    let st = shared.state.lock().await;
                    match &st.intercept {
                        Some(i) if i.patterns.iter().any(|p| p.matches(url)) => {
                            i.tx.send(ev.clone()).is_ok()
                        }
                        _ => false,
                    }
                };
                if !forwarded {
                    let _ = shared
                        .session
                        .send(fetch::ContinueRequest {
                            request_id: id.to_owned(),
                            ..Default::default()
                        })
                        .await;
                }
            }
            ev = event::next(&mut auth) => {
                let Some(ev) = ev else { break };
                let Some(id) = ev.params["requestId"].as_str() else { continue };
                let is_proxy = ev.params["authChallenge"]["source"].as_str() == Some("Proxy");
                let credentials = shared.state.lock().await.credentials.clone();
                let response = match (is_proxy, credentials) {
                    (true, Some(c)) => fetch::AuthChallengeResponse {
                        response: "ProvideCredentials".into(),
                        username: Some(c.username),
                        password: Some(c.password),
                    },
                    _ => fetch::AuthChallengeResponse {
                        response: "Default".into(),
                        username: None,
                        password: None,
                    },
                };
                let supplied = response.response == "ProvideCredentials";
                let r = shared
                    .session
                    .send(fetch::ContinueWithAuth {
                        request_id: id.to_owned(),
                        auth_challenge_response: response,
                    })
                    .await;
                if supplied && r.is_ok() {
                    tracing::debug!("proxy credentials supplied; auth need dropped");
                    shared.state.lock().await.credentials = None;
                    if let Err(e) = shared.reconfigure().await {
                        tracing::debug!("Fetch reconfigure after auth: {e}");
                    }
                }
            }
        }
    }
}

/// The receiving end of [`FetchHub::intercept`]. Dropping it stops
/// interception (and releases `Fetch` unless auth still needs it).
pub struct Interception {
    rx: mpsc::UnboundedReceiver<Event>,
    shared: Arc<FetchShared>,
}

impl std::fmt::Debug for Interception {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Interception").finish()
    }
}

impl Interception {
    /// The next paused request, or `None` once the session is gone.
    pub async fn next(&mut self) -> Option<InterceptedRequest> {
        let ev = self.rx.recv().await?;
        Some(InterceptedRequest::from_event(
            self.shared.session.clone(),
            &ev.params,
        ))
    }
}

impl Drop for Interception {
    fn drop(&mut self) {
        let shared = self.shared.clone();
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            shared.state.lock().await.intercept = None;
            if let Err(e) = shared.reconfigure().await {
                tracing::debug!("Fetch reconfigure after interception ended: {e}");
            }
        });
    }
}

/// Overrides for [`InterceptedRequest::continue_request`].
#[derive(Debug, Clone, Default)]
pub struct ContinueOverrides {
    /// Replace the URL (not observable by the page).
    pub url: Option<String>,
    /// Replace the method.
    pub method: Option<String>,
    /// Replace the request headers (all of them).
    pub headers: Option<Vec<(String, String)>>,
    /// Replace the POST body.
    pub post_data: Option<Vec<u8>>,
}

/// A request paused by `Fetch` for an `intercept(pattern):` handler. Exactly
/// one of `continue_request` / `fulfil` / `fail` must be called; the
/// runtime continues it untouched when the handler decides nothing.
#[derive(Debug)]
pub struct InterceptedRequest {
    session: Session,
    /// `Fetch.RequestId`.
    pub request_id: String,
    /// The request as the page issued it.
    pub request: RequestInfo,
    decided: AtomicBool,
}

impl InterceptedRequest {
    fn from_event(session: Session, p: &Value) -> InterceptedRequest {
        let mut request = RequestInfo::from_event(p);
        request.resource_type = p["resourceType"].as_str().unwrap_or("Other").to_owned();
        // `Fetch.requestPaused` carries its own id; `networkId` links to
        // the `Network` events.
        request.request_id = p["networkId"]
            .as_str()
            .unwrap_or_else(|| p["requestId"].as_str().unwrap_or(""))
            .to_owned();
        InterceptedRequest {
            session,
            request_id: p["requestId"].as_str().unwrap_or("").to_owned(),
            request,
            decided: AtomicBool::new(false),
        }
    }

    /// Whether a decision was already sent.
    pub fn is_decided(&self) -> bool {
        self.decided.load(Ordering::SeqCst)
    }

    fn decide(&self, what: &str) -> Result<(), BrowserError> {
        if self.decided.swap(true, Ordering::SeqCst) {
            return Err(BrowserError::Config {
                what: format!("intercept: {what}"),
                reason: "this request was already continued, fulfilled or failed".into(),
            });
        }
        Ok(())
    }

    /// `Fetch.continueRequest`, optionally rewritten.
    pub async fn continue_request(&self, o: ContinueOverrides) -> Result<(), BrowserError> {
        self.decide("continue")?;
        self.session
            .send(fetch::ContinueRequest {
                request_id: self.request_id.clone(),
                url: o.url,
                method: o.method,
                post_data: o.post_data.as_deref().map(base64_encode),
                headers: o.headers.map(header_entries),
                intercept_response: None,
            })
            .await?;
        Ok(())
    }

    /// `Fetch.fulfillRequest` with a synthetic response.
    pub async fn fulfil(
        &self,
        status: i64,
        headers: Vec<(String, String)>,
        body: &[u8],
    ) -> Result<(), BrowserError> {
        self.decide("fulfil")?;
        self.session
            .send(fetch::FulfillRequest {
                request_id: self.request_id.clone(),
                response_code: status,
                response_headers: Some(header_entries(headers)),
                binary_response_headers: None,
                body: Some(base64_encode(body)),
                response_phrase: None,
            })
            .await?;
        Ok(())
    }

    /// `Fetch.failRequest` with a `Network.ErrorReason` (`Failed`,
    /// `Aborted`, `TimedOut`, `AccessDenied`, `ConnectionRefused`,
    /// `NameNotResolved`, `BlockedByClient`, …).
    pub async fn fail(&self, reason: &str) -> Result<(), BrowserError> {
        self.decide("fail")?;
        let reason: network::ErrorReason = serde_json::from_value(Value::String(reason.to_owned()))
            .map_err(|_| BrowserError::Config {
                what: "intercept: fail reason".into(),
                reason: format!(
                    "{reason:?} is not a Network.ErrorReason (Failed, Aborted, TimedOut, \
                         AccessDenied, ConnectionClosed, ConnectionReset, ConnectionRefused, \
                         ConnectionAborted, ConnectionFailed, NameNotResolved, \
                         InternetDisconnected, AddressUnreachable, BlockedByClient, \
                         BlockedByResponse)"
                ),
            })?;
        self.session
            .send(fetch::FailRequest {
                request_id: self.request_id.clone(),
                error_reason: reason,
            })
            .await?;
        Ok(())
    }
}

fn header_entries(headers: Vec<(String, String)>) -> Vec<fetch::HeaderEntry> {
    headers
        .into_iter()
        .map(|(name, value)| fetch::HeaderEntry { name, value })
        .collect()
}

// ───────────────────────── downloads ─────────────────────────

/// A download seen by the [`DownloadTracker`].
#[derive(Debug, Clone)]
pub struct Download {
    /// Chrome's download guid (also the on-disk name until completion).
    pub guid: String,
    /// Initiating frame.
    pub frame_id: String,
    /// Source URL.
    pub url: String,
    /// Name the server / page suggested.
    pub suggested_filename: String,
    /// `inProgress`, `completed`, `canceled`.
    pub state: String,
    /// Final path once completed (renamed to the suggested name).
    pub path: Option<PathBuf>,
    /// Handed out by `wait_download()` already.
    pub consumed: bool,
}

/// Follows `Browser.downloadWillBegin` / `Browser.downloadProgress` on the
/// root session and renames finished files; see the module docs.
pub struct DownloadTracker {
    dir: PathBuf,
    downloads: Arc<Mutex<Vec<Download>>>,
    notify: Arc<Notify>,
    task: JoinHandle<()>,
}

impl std::fmt::Debug for DownloadTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DownloadTracker")
            .field("dir", &self.dir)
            .finish()
    }
}

impl DownloadTracker {
    /// Create `dir`, subscribe on `root` and send the default context's
    /// `Browser.setDownloadBehavior`.
    pub async fn install(root: Session, dir: &Path) -> Result<DownloadTracker, BrowserError> {
        std::fs::create_dir_all(dir)?;
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let begin = root.events("Browser.downloadWillBegin");
        let progress = root.events("Browser.downloadProgress");
        set_download_behavior(&root, &dir, None).await?;
        let downloads = Arc::new(Mutex::new(Vec::new()));
        let notify = Arc::new(Notify::new());
        let task = tokio::spawn(run_tracker(
            dir.clone(),
            downloads.clone(),
            notify.clone(),
            begin,
            progress,
        ));
        Ok(DownloadTracker {
            dir,
            downloads,
            notify,
            task,
        })
    }

    /// The download directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `Browser.setDownloadBehavior` for a browser context created later.
    pub async fn apply_to_context(
        &self,
        root: &Session,
        context_id: &str,
    ) -> Result<(), BrowserError> {
        set_download_behavior(root, &self.dir, Some(context_id)).await
    }

    /// Every download seen so far.
    pub fn downloads(&self) -> Vec<Download> {
        self.downloads.lock().expect("downloads").clone()
    }

    /// Wait for the next unconsumed download started by `frame_id` to
    /// complete; returns its final path.
    pub async fn wait(&self, frame_id: &str, timeout: Duration) -> Result<PathBuf, BrowserError> {
        let started = Instant::now();
        loop {
            {
                let mut list = self.downloads.lock().expect("downloads");
                if let Some(d) = list
                    .iter_mut()
                    .find(|d| !d.consumed && d.frame_id == frame_id && d.state != "inProgress")
                {
                    d.consumed = true;
                    return match (d.state.as_str(), d.path.clone()) {
                        ("completed", Some(p)) => Ok(p),
                        (state, _) => Err(BrowserError::Unsupported(format!(
                            "download of {} was {state}",
                            d.url
                        ))),
                    };
                }
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                let pending = self
                    .downloads
                    .lock()
                    .expect("downloads")
                    .iter()
                    .any(|d| !d.consumed && d.frame_id == frame_id);
                return Err(BrowserError::Timeout {
                    action: "wait_download".into(),
                    selector: None,
                    waited_ms: started.elapsed().as_millis() as u64,
                    last_state: Some(if pending {
                        "a download is still in progress".into()
                    } else {
                        "no download started on this page".into()
                    }),
                });
            }
            let _ = tokio::time::timeout(
                remaining.min(Duration::from_millis(100)),
                self.notify.notified(),
            )
            .await;
        }
    }
}

impl Drop for DownloadTracker {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn set_download_behavior(
    root: &Session,
    dir: &Path,
    context_id: Option<&str>,
) -> Result<(), BrowserError> {
    let mut params = json!({
        "behavior": "allowAndName",
        "downloadPath": dir.to_string_lossy(),
        "eventsEnabled": true,
    });
    if let Some(ctx) = context_id {
        params["browserContextId"] = json!(ctx);
    }
    root.call_raw("Browser.setDownloadBehavior", params).await?;
    Ok(())
}

async fn run_tracker(
    dir: PathBuf,
    downloads: Arc<Mutex<Vec<Download>>>,
    notify: Arc<Notify>,
    mut begin: broadcast::Receiver<Event>,
    mut progress: broadcast::Receiver<Event>,
) {
    loop {
        tokio::select! {
            ev = event::next(&mut begin) => {
                let Some(ev) = ev else { break };
                let p = &ev.params;
                let d = Download {
                    guid: p["guid"].as_str().unwrap_or("").to_owned(),
                    frame_id: p["frameId"].as_str().unwrap_or("").to_owned(),
                    url: p["url"].as_str().unwrap_or("").to_owned(),
                    suggested_filename: p["suggestedFilename"].as_str().unwrap_or("download").to_owned(),
                    state: "inProgress".into(),
                    path: None,
                    consumed: false,
                };
                tracing::debug!("download {} of {} began", d.guid, d.url);
                downloads.lock().expect("downloads").push(d);
            }
            ev = event::next(&mut progress) => {
                let Some(ev) = ev else { break };
                let p = &ev.params;
                let guid = p["guid"].as_str().unwrap_or("");
                let state = p["state"].as_str().unwrap_or("inProgress");
                if state == "inProgress" {
                    continue;
                }
                let suggested = downloads
                    .lock()
                    .expect("downloads")
                    .iter()
                    .find(|d| d.guid == guid)
                    .map(|d| d.suggested_filename.clone());
                let Some(suggested) = suggested else { continue };
                let path = if state == "completed" {
                    let from = dir.join(guid);
                    let to = unique_path(&dir, &suggested);
                    match tokio::fs::rename(&from, &to).await {
                        Ok(()) => Some(to),
                        Err(e) => {
                            tracing::debug!("renaming download {}: {e}", from.display());
                            Some(from)
                        }
                    }
                } else {
                    None
                };
                let mut list = downloads.lock().expect("downloads");
                if let Some(d) = list.iter_mut().find(|d| d.guid == guid) {
                    d.state = state.to_owned();
                    d.path = path;
                }
                drop(list);
                notify.notify_waiters();
            }
        }
    }
}

/// `dir/name`, or `dir/name (n).ext` when it exists already.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let safe = Path::new(name)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .filter(|f| !f.is_empty())
        .unwrap_or_else(|| "download".into());
    let first = dir.join(&safe);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match safe.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_owned(), format!(".{e}")),
        _ => (safe.clone(), String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("unbounded")
}

/// Header map as `name → value` for scripts.
pub fn headers_map(headers: &[(String, String)]) -> HashMap<String, String> {
    headers.iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, url: &str) -> bool {
        UrlPattern::parse(p).unwrap().matches(url)
    }

    #[test]
    fn globs_are_anchored_and_star_spans_slashes() {
        assert!(m("*/api/*", "https://x.com/api/v1"));
        assert!(m("https://x.com/*", "https://x.com/"));
        assert!(!m("https://y.com/*", "https://x.com/"));
        assert!(m("*.png", "https://x.com/a.png"));
        assert!(!m("*.png", "https://x.com/a.jpg"));
        assert!(!m("*.png", "https://x.com/a.png?x=1"));
        assert!(m("**/api/**", "https://x.com/api/v1/items"));
        assert!(m("*", ""));
        assert!(m("", ""));
        assert!(!m("", "x"));
        assert!(m("https://x.com/a", "https://x.com/a"));
        assert!(!m("https://x.com/a", "https://x.com/ab"));
        assert!(m("*://ads.*/*", "http://ads.example.com/banner.js"));
        assert!(!m("*://ads.*/*", "http://example.com/ads/banner.js"));
    }

    #[test]
    fn regex_patterns() {
        assert!(m("re:^https?://x\\.com/(a|b)$", "http://x.com/b"));
        assert!(!m("re:^https?://x\\.com/(a|b)$", "http://x.com/c"));
        assert!(UrlPattern::parse("re:(").is_err());
        assert_eq!(UrlPattern::parse("re:x").unwrap().chrome_pattern(), "*");
        assert_eq!(
            UrlPattern::parse("**/a/*.png").unwrap().chrome_pattern(),
            "*/a/*.png"
        );
    }

    #[test]
    fn headers_are_lower_cased() {
        let h = headers_of(&json!({"Content-Type": "text/html", "X-N": 1}));
        assert_eq!(
            h,
            vec![
                ("content-type".to_string(), "text/html".to_string()),
                ("x-n".to_string(), "1".to_string())
            ]
        );
    }

    #[test]
    fn unique_paths_add_a_counter() {
        let dir = tempfile::tempdir().unwrap();
        let a = unique_path(dir.path(), "report.txt");
        assert_eq!(a, dir.path().join("report.txt"));
        std::fs::write(&a, "x").unwrap();
        assert_eq!(
            unique_path(dir.path(), "report.txt"),
            dir.path().join("report (1).txt")
        );
        assert_eq!(unique_path(dir.path(), "../evil"), dir.path().join("evil"));
    }
}
