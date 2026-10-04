//! End-to-end tests against a real Chrome.
//!
//! Skip (with a printed message) when no Chrome is found; `SURF_CHROME`
//! overrides discovery. These MUST run on developer Macs and in the `e2e`
//! CI job. Task 9 fills in the suite (local axum fixture server, quiet-flag
//! assertions, `Runtime.bindingCalled` without `Runtime.enable`, …).

use surf_browser::discovery::chrome_or_skip;

#[test]
fn chrome_is_discoverable() {
    let Some(path) = chrome_or_skip("chrome_is_discoverable") else {
        return;
    };
    assert!(path.is_file(), "{} is not a file", path.display());
}

#[test]
fn examples_are_present() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    for name in [
        "hello.surf",
        "login.surf",
        "two-tabs.surf",
        "scrape-emit.surf",
        "parallel-pool.surf",
        "supervised.surf",
    ] {
        let p = std::path::Path::new(dir).join(name);
        assert!(p.is_file(), "missing example {}", p.display());
    }
}
