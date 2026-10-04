//! # surf-testserver
//!
//! An in-process `axum` fixture server used by `surf-browser`'s integration
//! tests and by the `surf` e2e suite (`tests/e2e/main.rs`, scripts in
//! `tests/e2e/scripts`). It serves `fixtures/*.html` from this crate plus
//! a few dynamic endpoints:
//!
//! | route | behaviour |
//! |-------|-----------|
//! | `/` | `<h1>Index</h1>` |
//! | `/<file>.html` | the fixture file |
//! | `/cookie-echo` | the request's `Cookie` header as `text/plain` |
//! | `/set-cookie?name=value` | sets `name=value; Path=/` |
//! | `/slow?ms=N` | answers after `N` ms |
//! | `/proxy-echo` | the request's `X-Surf-Proxy` header (`none` without one) as `text/plain` |
//! | `/redirect?n=N` | 302 chain of `N` hops ending at `/redirected` |
//! | `/redirected` | the landing page of the chain |
//! | `/api/items` | JSON `{"items": [...]}` |
//! | `/api/other` | a different JSON list (for `continue(url:)`) |
//! | `/api/echo` | JSON echo of method, path, query, `x-*` headers and body |
//! | `/img/pixel.png` | a 1×1 PNG (for `block` / `intercept`) |
//! | `/ads/banner.js` | a script that sets `window.adLoaded` |
//! | `/download/report.txt` | `Content-Disposition: attachment` |
//! | `/detector` | `tools/detector/index.html` from the workspace (`?mode=headed\|headless`) |
//!
//! [`proxy::Proxy`] is a forward-proxy stub (optionally requiring Basic
//! authentication) that tags what it forwards with `X-Surf-Proxy` on the
//! request and `X-Proxy` on the response, so a script can prove which proxy
//! a page went through.
//!
//! Not published; a dev-dependency only.

#![forbid(unsafe_code)]

pub mod proxy;
pub use proxy::Proxy;

use axum::body::Bytes;
use axum::extract::Query;
use axum::http::{header, HeaderMap, Method, StatusCode, Uri};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{any, get};
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
        .route("/redirect", get(redirect))
        .route("/redirected", get(redirected))
        .route("/api/items", get(api_items))
        .route("/api/other", get(api_other))
        .route("/api/echo", any(api_echo))
        .route("/img/pixel.png", get(pixel))
        .route("/ads/banner.js", get(banner))
        .route("/download/report.txt", get(download))
        .route("/detector", get(detector))
        .route("/{file}", get(file))
}

/// Directory holding the `*.html` fixtures.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// The workspace root (`tools/detector/index.html` lives there).
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
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

/// `/redirect?n=N`: 302 to `n-1` until `/redirected`.
async fn redirect(Query(q): Query<HashMap<String, String>>) -> Response {
    let n: u32 = q.get("n").and_then(|s| s.parse().ok()).unwrap_or(1);
    if n <= 1 {
        Redirect::to("/redirected").into_response()
    } else {
        Redirect::to(&format!("/redirect?n={}", n - 1)).into_response()
    }
}

async fn redirected() -> Html<&'static str> {
    Html("<!doctype html><title>Redirected</title><h1 id=\"redirected\">landed</h1>")
}

fn json(body: String) -> Response {
    ([(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn api_items() -> Response {
    json(
        r#"{"items":[{"id":1,"name":"one"},{"id":2,"name":"two"},{"id":3,"name":"three"}]}"#.into(),
    )
}

async fn api_other() -> Response {
    json(r#"{"items":[{"id":99,"name":"other"}]}"#.into())
}

/// JSON echo: `{method, path, query, headers: {x-*: …, content-type}, body}`.
async fn api_echo(method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let mut hdrs = serde_json::Map::new();
    for (k, v) in &headers {
        let name = k.as_str().to_ascii_lowercase();
        if name.starts_with("x-") || name == "content-type" {
            hdrs.insert(
                name,
                serde_json::Value::String(v.to_str().unwrap_or("").to_owned()),
            );
        }
    }
    let v = serde_json::json!({
        "method": method.as_str(),
        "path": uri.path(),
        "query": uri.query().unwrap_or(""),
        "headers": hdrs,
        "body": String::from_utf8_lossy(&body),
    });
    json(v.to_string())
}

/// A 1×1 transparent PNG.
const PIXEL: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x60, 0x00, 0x02, 0x00,
    0x00, 0x05, 0x00, 0x01, 0xe2, 0x26, 0x05, 0x9b, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
    0xae, 0x42, 0x60, 0x82,
];

async fn pixel() -> Response {
    ([(header::CONTENT_TYPE, "image/png")], PIXEL).into_response()
}

async fn banner() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript")],
        "window.adLoaded = true; document.getElementById('ad').textContent = 'ad loaded';",
    )
        .into_response()
}

/// `Content-Disposition: attachment; filename="report.txt"`.
async fn download() -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"report.txt\"",
            ),
        ],
        "line one\nline two\n",
    )
        .into_response()
}

/// `tools/detector/index.html` from the workspace root (the local
/// detection gate; see `docs/quiet-cdp.md`).
async fn detector() -> Response {
    let path = workspace_root().join("tools/detector/index.html");
    match std::fs::read_to_string(&path) {
        Ok(body) => Html(body).into_response(),
        Err(_) => (
            StatusCode::NOT_FOUND,
            format!("detector page not built yet (expected {})", path.display()),
        )
            .into_response(),
    }
}
