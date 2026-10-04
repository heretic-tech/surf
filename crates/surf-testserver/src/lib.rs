//! # surf-testserver
//!
//! An in-process `axum` fixture server used by `surf-browser`'s integration
//! tests and by the `surf` e2e suite (`crates/surf-cli/tests/e2e.rs`,
//! scripts in `tests/e2e/scripts`). It serves `fixtures/*.html` from this
//! crate plus a few dynamic endpoints:
//!
//! | route | behaviour |
//! |-------|-----------|
//! | `/` | `<h1>Index</h1>` |
//! | `/<file>.html` | the fixture file |
//! | `/cookie-echo` | the request's `Cookie` header as `text/plain` |
//! | `/set-cookie?name=value` | sets `name=value; Path=/` |
//! | `/slow?ms=N` | answers after `N` ms |
//! | `/proxy-echo` | the request's `X-Surf-Proxy` header (`none` without one) as `text/plain` |
//!
//! [`proxy::Proxy`] is a forward-proxy stub that tags what it forwards with
//! `X-Surf-Proxy`, so a script can prove which proxy a page went through.
//!
//! Not published; a dev-dependency only.

#![forbid(unsafe_code)]

pub mod proxy;
pub use proxy::Proxy;

use axum::extract::Query;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// A running fixture server. Dropping it aborts the server task.
pub struct Fixture {
    /// `http://127.0.0.1:<port>`
    pub base: String,
    task: JoinHandle<()>,
}

impl Fixture {
    /// Bind a free loopback port and serve. Must be called inside a tokio
    /// runtime; the server runs on a spawned task until the `Fixture` is
    /// dropped.
    pub async fn start() -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router()).await;
        });
        Fixture {
            base: format!("http://{addr}"),
            task,
        }
    }

    /// Absolute URL for `path` (leading `/`).
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The fixture router (for callers that want to serve it themselves).
pub fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/cookie-echo", get(cookie_echo))
        .route("/set-cookie", get(set_cookie))
        .route("/slow", get(slow))
        .route("/proxy-echo", get(proxy_echo))
        .route("/{file}", get(file))
}

/// Directory holding the `*.html` fixtures.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

async fn index() -> Html<&'static str> {
    Html("<!doctype html><title>Index</title><h1>Index</h1>")
}

async fn file(axum::extract::Path(name): axum::extract::Path<String>) -> Response {
    if name.contains("..") || name.contains('/') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = fixtures_dir().join(&name);
    match std::fs::read_to_string(&path) {
        Ok(body) => Html(body).into_response(),
        Err(_) => (StatusCode::NOT_FOUND, format!("no fixture {name}")).into_response(),
    }
}

/// Echo the request's `Cookie` header as `text/plain` (empty when none).
async fn cookie_echo(headers: HeaderMap) -> Response {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        cookie,
    )
        .into_response()
}

/// `/set-cookie?name=value` sets `name=value; Path=/`.
async fn set_cookie(Query(q): Query<HashMap<String, String>>) -> Response {
    let mut headers = HeaderMap::new();
    for (k, v) in &q {
        if let Ok(hv) = format!("{k}={v}; Path=/").parse() {
            headers.append(header::SET_COOKIE, hv);
        }
    }
    (headers, Html("<!doctype html><title>cookie set</title>ok")).into_response()
}

/// Echo the `X-Surf-Proxy` header a [`Proxy`] stub adds (`none` without).
async fn proxy_echo(headers: HeaderMap) -> Response {
    let via = headers
        .get("x-surf-proxy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("none")
        .to_owned();
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], via).into_response()
}

/// `/slow?ms=N` answers after `N` ms.
async fn slow(Query(q): Query<HashMap<String, String>>) -> Html<String> {
    let ms: u64 = q.get("ms").and_then(|s| s.parse().ok()).unwrap_or(500);
    tokio::time::sleep(Duration::from_millis(ms)).await;
    Html(format!(
        "<!doctype html><title>slow</title><div id='slow'>slept {ms}</div>"
    ))
}
