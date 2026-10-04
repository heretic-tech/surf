//! Request / response hooks, blocking, interception, proxy auth.
//!
//! `Network.enable` / `Fetch.enable` are held by `DomainGuard`s owned by the
//! registered hooks; when the last hook is removed the domain is disabled.
//! Hooks / blocking / intercept arrive in task 9; proxy authentication
//! ([`ProxyAuth`]) lives here already because pages need it.
//!
//! ## Proxy authentication
//!
//! Credentials never go on the command line (`--proxy-server` carries only
//! `scheme://host:port`). Instead, each page whose browser context has an
//! authenticating proxy gets `Fetch.enable{handleAuthRequests: true,
//! patterns: [{urlPattern: "*"}]}` on its session; a task answers
//! `Fetch.requestPaused` with `Fetch.continueRequest` and
//! `Fetch.authRequired` with `Fetch.continueWithAuth{ProvideCredentials}`
//! when `authChallenge.source == "Proxy"` (`Default` otherwise, so a site's
//! own 401 still reaches the page). Chrome caches the proxy credentials for
//! the context, so after the first answered challenge the `Fetch`
//! [`DomainGuard`] is dropped — `Fetch.disable` goes out unless a network
//! hook still holds the domain (ref-counted).

use crate::error::BrowserError;
use crate::launch::Credentials;
use surf_cdp::protocol::fetch;
use surf_cdp::{event, DomainGuard, Session};
use tokio::task::JoinHandle;

/// A URL pattern (`*` glob) used by `on request(pattern)` and friends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlPattern(pub String);

impl UrlPattern {
    /// Whether `url` matches (glob `*` wildcards).
    pub fn matches(&self, url: &str) -> bool {
        glob_match(&self.0, url)
    }
}

/// Answers proxy challenges on one page session; see the module docs.
/// Dropping it aborts the task and releases the `Fetch` guard.
#[derive(Debug)]
pub struct ProxyAuth {
    task: JoinHandle<()>,
}

impl ProxyAuth {
    /// Enable `Fetch` on `session` and start answering challenges with
    /// `credentials`.
    pub async fn install(
        session: Session,
        credentials: Credentials,
    ) -> Result<ProxyAuth, BrowserError> {
        // Subscribe before enabling so no event is missed.
        let paused = session.events("Fetch.requestPaused");
        let auth = session.events("Fetch.authRequired");
        let guard = session.enable_domain("Fetch").await?;
        // `enable_domain` sends a bare `Fetch.enable`; re-send with the
        // auth flag (idempotent: the last `enable` wins).
        session
            .send(fetch::Enable {
                patterns: Some(vec![fetch::RequestPattern {
                    url_pattern: Some("*".into()),
                    resource_type: None,
                    request_stage: None,
                }]),
                handle_auth_requests: Some(true),
            })
            .await?;
        let task = tokio::spawn(run(session, credentials, guard, paused, auth));
        Ok(ProxyAuth { task })
    }
}

impl Drop for ProxyAuth {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run(
    session: Session,
    credentials: Credentials,
    guard: DomainGuard,
    mut paused: tokio::sync::broadcast::Receiver<surf_cdp::Event>,
    mut auth: tokio::sync::broadcast::Receiver<surf_cdp::Event>,
) {
    let mut guard = Some(guard);
    loop {
        tokio::select! {
            ev = event::next(&mut paused) => {
                let Some(ev) = ev else { break };
                let Some(id) = ev.params["requestId"].as_str() else { continue };
                let _ = session
                    .send(fetch::ContinueRequest {
                        request_id: id.to_owned(),
                        ..Default::default()
                    })
                    .await;
            }
            ev = event::next(&mut auth) => {
                let Some(ev) = ev else { break };
                let Some(id) = ev.params["requestId"].as_str() else { continue };
                let is_proxy = ev.params["authChallenge"]["source"].as_str() == Some("Proxy");
                let response = if is_proxy {
                    fetch::AuthChallengeResponse {
                        response: "ProvideCredentials".into(),
                        username: Some(credentials.username.clone()),
                        password: Some(credentials.password.clone()),
                    }
                } else {
                    fetch::AuthChallengeResponse {
                        response: "Default".into(),
                        username: None,
                        password: None,
                    }
                };
                let r = session
                    .send(fetch::ContinueWithAuth {
                        request_id: id.to_owned(),
                        auth_challenge_response: response,
                    })
                    .await;
                if is_proxy && r.is_ok() {
                    tracing::debug!("proxy credentials supplied; releasing Fetch");
                    // Chrome caches them for the context; stop pausing
                    // requests. Events already queued are still answered
                    // below until the channels close.
                    guard.take();
                }
            }
        }
    }
    drop(guard);
}

fn glob_match(pat: &str, s: &str) -> bool {
    let mut parts = pat.split('*');
    let first = parts.next().unwrap_or("");
    if !s.starts_with(first) {
        return false;
    }
    let mut rest = &s[first.len()..];
    let mut last_part: Option<&str> = None;
    for p in parts {
        last_part = Some(p);
        match rest.find(p) {
            Some(i) => rest = &rest[i + p.len()..],
            None => return false,
        }
    }
    match last_part {
        None => rest.is_empty(),
        Some(p) => p.is_empty() || s.ends_with(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(UrlPattern("*/api/*".into()).matches("https://x.com/api/v1"));
        assert!(UrlPattern("https://x.com/*".into()).matches("https://x.com/"));
        assert!(!UrlPattern("https://y.com/*".into()).matches("https://x.com/"));
        assert!(UrlPattern("*.png".into()).matches("https://x.com/a.png"));
        assert!(!UrlPattern("*.png".into()).matches("https://x.com/a.jpg"));
    }
}
