//! Shared test support: an in-process axum fixture server serving
//! `tests/fixtures/*.html` plus a few dynamic endpoints, and a helper that
//! launches a headless Chrome (or prints a skip message).

#![allow(dead_code)]

use axum::extract::Query;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;
use surf_browser::discovery::chrome_or_skip;
use surf_browser::{Browser, LaunchOptions};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

/// A running fixture server.
pub struct Fixture {
    /// `http://127.0.0.1:<port>`
    pub base: String,
    task: JoinHandle<()>,
}

impl Fixture {
    /// Bind a free loopback port and serve.
    pub async fn start() -> Fixture {
        let app = Router::new()
            .route("/", get(index))
            .route("/cookie-echo", get(cookie_echo))
            .route("/set-cookie", get(set_cookie))
            .route("/slow", get(slow))
            .route("/{file}", get(file));
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
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

async fn index() -> Html<&'static str> {
    Html("<!doctype html><title>Index</title><h1>Index</h1>")
}

async fn file(axum::extract::Path(name): axum::extract::Path<String>) -> Response {
    if name.contains("..") || name.contains('/') {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(&name);
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

/// `/slow?ms=N` answers after `N` ms.
async fn slow(Query(q): Query<HashMap<String, String>>) -> Html<String> {
    let ms: u64 = q.get("ms").and_then(|s| s.parse().ok()).unwrap_or(500);
    tokio::time::sleep(Duration::from_millis(ms)).await;
    Html(format!(
        "<!doctype html><title>slow</title><div id='slow'>slept {ms}</div>"
    ))
}

/// Launch a headless browser for `test`, or `None` (with a printed skip
/// message) when no Chrome is available.
pub async fn browser(test: &str) -> Option<Rc<Browser>> {
    let chrome = chrome_or_skip(test)?;
    let opts = LaunchOptions {
        path: Some(chrome),
        headless: Some(true),
        timeout: Duration::from_secs(10),
        ..Default::default()
    };
    Some(Browser::launch(opts).await.expect("launch"))
}
