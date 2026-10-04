//! `surf-testserver [port]` — serve the fixtures on loopback and print the
//! base URL, for running examples and e2e scripts by hand:
//!
//! ```text
//! $ cargo run -q -p surf-testserver -- 8080
//! http://127.0.0.1:8080
//! $ SURF_E2E_BASE=http://127.0.0.1:8080 surf tests/e2e/scripts/login.surf
//! ```

#![forbid(unsafe_code)]

use tokio::net::TcpListener;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind loopback port");
    let addr = listener.local_addr().expect("local addr");
    println!("http://{addr}");
    if let Err(e) = axum::serve(listener, surf_testserver::router()).await {
        eprintln!("surf-testserver: {e}");
        std::process::exit(1);
    }
}
