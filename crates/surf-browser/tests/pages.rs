//! Pages, isolated worlds, auto-waiting actions, selectors and browser
//! contexts against a real headless Chrome and the in-process fixture
//! server. Every test skips (printing a message) when no Chrome is found;
//! `SURF_CHROME` overrides discovery.
//!
//! The last test traces every frame and asserts the quiet contract:
//! `Page.enable` is the only `*.enable` ever sent — never `Runtime.enable`,
//! never `DOM.enable`.

mod common;

use common::{browser, Fixture};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use surf_browser::{
    ActionOptions, BrowserError, Cookie, DialogPolicy, FrameRef, Migration, NewPageOptions,
    RebindTarget, WaitUntil,
};

fn opts() -> ActionOptions {
    ActionOptions::default()
}

fn quick() -> ActionOptions {
    ActionOptions::timeout(Duration::from_millis(600))
}

#[tokio::test]
async fn goto_eval_readers_and_navigation() {
    let Some(b) = browser("goto_eval_readers_and_navigation").await else {
        return;
    };
    let fx = Fixture::start().await;
    let page = b.sole_page().await.expect("sole page");
    assert_eq!(page.index(), 1);

    page.goto(&fx.url("/forms.html"), WaitUntil::Load)
        .await
        .expect("goto");
    assert_eq!(page.title().await.unwrap(), "Forms fixture");
    assert_eq!(page.url().await.unwrap(), fx.url("/forms.html"));
    assert_eq!(page.eval("1 + 2").await.unwrap(), json!(3));
    assert_eq!(
        page.eval("Promise.resolve(document.title)").await.unwrap(),
        json!("Forms fixture")
    );
    assert_eq!(
        page.eval_fn("function(a, b) { return a * b; }", vec![json!(6), json!(7)])
            .await
            .unwrap(),
        json!(42)
    );
    let err = page.eval("nope.x").await.unwrap_err();
    assert!(
        matches!(err, BrowserError::Script { .. }),
        "expected Script error, got {err}"
    );
    assert!(err.to_string().contains("ReferenceError"), "{err}");

    // Readers.
    assert_eq!(page.text("h1", opts()).await.unwrap(), "Forms");
    assert_eq!(page.text("#para", opts()).await.unwrap(), "Hello world");
    assert_eq!(
        page.html("#para", opts()).await.unwrap(),
        "<p id=\"para\">  Hello <b>world</b>  </p>"
    );
    assert_eq!(
        page.attr("#link", "data-kind", opts()).await.unwrap(),
        Some("internal".into())
    );
    assert_eq!(page.attr("#link", "nope", opts()).await.unwrap(), None);
    assert_eq!(page.value("#notes", opts()).await.unwrap(), "old notes");
    assert!(page.exists("#username").await.unwrap());
    assert!(!page.exists("#missing").await.unwrap());
    assert_eq!(page.count("input[type=radio]").await.unwrap(), 2);
    assert_eq!(page.count("xpath=//select/option").await.unwrap(), 7);
    assert_eq!(page.count("//select").await.unwrap(), 2);

    // all() + Element readers.
    let links = page.all("a").await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].text().await.unwrap(), "go to nav");
    assert_eq!(
        links[0].attr("href").await.unwrap(),
        Some("/nav.html".into())
    );
    let options = page.all("#color option").await.unwrap();
    assert_eq!(options.len(), 4);
    assert_eq!(options[1].attr("value").await.unwrap(), Some("red".into()));
    let form = page.first("#login").await.unwrap().expect("form");
    assert_eq!(form.all("select").await.unwrap().len(), 2);
    assert_eq!(form.all("text=Log in").await.unwrap().len(), 1);

    // Navigation: link click → wait_url, back, forward, reload.
    page.click("#link", opts()).await.expect("click link");
    page.wait_url("*/nav.html", opts()).await.expect("wait_url");
    assert_eq!(page.text("#nav-title", opts()).await.unwrap(), "Navigated");
    assert!(page.back(WaitUntil::Load).await.unwrap());
    page.wait_url("re:/forms\\.html$", opts()).await.unwrap();
    assert_eq!(page.title().await.unwrap(), "Forms fixture");
    assert!(page.forward(WaitUntil::Load).await.unwrap());
    assert_eq!(page.title().await.unwrap(), "Nav fixture");
    assert!(!page.forward(WaitUntil::Load).await.unwrap());
    page.reload(WaitUntil::DomContentLoaded).await.unwrap();
    assert_eq!(page.text("#nav-title", opts()).await.unwrap(), "Navigated");

    // Fragment navigation is same-document: no load event to wait for.
    let target = format!("{}#section", fx.url("/nav.html"));
    page.goto(&target, WaitUntil::Load).await.unwrap();
    assert_eq!(page.url().await.unwrap(), target);

    // A refused connection is a Navigation error, not a hang.
    let err = page
        .goto("http://127.0.0.1:1/", WaitUntil::Load)
        .await
        .unwrap_err();
    assert!(
        matches!(err, BrowserError::Navigation { .. }),
        "expected Navigation error, got {err}"
    );

    b.close().await.unwrap();
}

/// `click` arms the navigation subscriptions before it runs, so a
/// `wait_for_navigation` issued after a navigation has *already*
/// committed and loaded (same-origin, local server, plus a deliberate
/// pause) still succeeds instead of waiting for a navigation that will
/// never come. Without a preceding action the wait only sees later
/// navigations.
#[tokio::test]
async fn wait_for_navigation_sees_navigation_committed_before_the_call() {
    let Some(b) = browser("wait_for_navigation_sees_navigation_committed_before_the_call").await
    else {
        return;
    };
    let fx = Fixture::start().await;
    let page = b.sole_page().await.expect("sole page");
    page.goto(&fx.url("/forms.html"), WaitUntil::Load)
        .await
        .expect("goto");

    // click → navigation completes long before wait_for_navigation runs.
    page.click("#link", opts()).await.expect("click link");
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(page.url().await.unwrap().ends_with("/nav.html"));
    let started = Instant::now();
    page.wait_for_navigation(WaitUntil::Load)
        .await
        .expect("wait_for_navigation after an already-finished navigation");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "should return from buffered events, took {:?}",
        started.elapsed()
    );
    assert_eq!(page.title().await.unwrap(), "Nav fixture");

    // The armed subscriptions are consumed: a second wait with no action
    // in between times out (nothing navigates).
    let default_timeout = page.timeout();
    page.set_timeout(Duration::from_millis(800));
    let err = page
        .wait_for_navigation(WaitUntil::Commit)
        .await
        .unwrap_err();
    assert!(
        matches!(err, BrowserError::Timeout { .. }),
        "expected Timeout, got {err}"
    );
    page.set_timeout(default_timeout);

    // `commit` after an already-committed navigation returns at once.
    page.click("#back-link", opts())
        .await
        .expect("click back-link");
    tokio::time::sleep(Duration::from_millis(400)).await;
    page.wait_for_navigation(WaitUntil::Commit).await.unwrap();
    assert!(page.url().await.unwrap().ends_with("/forms.html"));

    // `press` arms too (Enter on a focused link navigates).
    page.focus("#link", opts()).await.unwrap();
    page.press("Enter").await.unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    page.wait_for_navigation(WaitUntil::Load).await.unwrap();
    assert_eq!(page.title().await.unwrap(), "Nav fixture");

    b.close().await.unwrap();
}

#[tokio::test]
async fn form_actions() {
    let Some(b) = browser("form_actions").await else {
        return;
    };
    let fx = Fixture::start().await;
    let page = b.page(1).await.unwrap();
    page.goto(&fx.url("/forms.html"), WaitUntil::Load)
        .await
        .unwrap();

    page.type_text("#username", "ali ce!", opts())
        .await
        .unwrap();
    assert_eq!(page.value("#username", opts()).await.unwrap(), "ali ce!");
    page.type_text("#username", "é", opts()).await.unwrap(); // insertText path
    assert_eq!(page.value("#username", opts()).await.unwrap(), "ali ce!é");
    page.fill("#username", "bob", opts()).await.unwrap();
    assert_eq!(page.value("#username", opts()).await.unwrap(), "bob");
    page.fill("#username", "", opts()).await.unwrap();
    assert_eq!(page.value("#username", opts()).await.unwrap(), "");
    page.fill("#notes", "new notes", opts()).await.unwrap();
    assert_eq!(page.value("#notes", opts()).await.unwrap(), "new notes");
    page.fill("#editable", "typed here", opts()).await.unwrap();
    assert_eq!(page.text("#editable", opts()).await.unwrap(), "typed here");

    // Typing with a delay still lands every key.
    let started = Instant::now();
    page.fill("#username", "", opts()).await.unwrap();
    page.type_text(
        "#username",
        "abcd",
        ActionOptions {
            delay: Some(Duration::from_millis(40)),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(page.value("#username", opts()).await.unwrap(), "abcd");
    assert!(started.elapsed() >= Duration::from_millis(60));

    // press: Enter on the input, a Ctrl combo, Shift+Tab, Backspace.
    page.press_on("#username", "Enter", opts()).await.unwrap();
    assert_eq!(page.text("#submitted", opts()).await.unwrap(), "enter");
    page.press("Ctrl+b").await.unwrap();
    page.press("Shift+Tab").await.unwrap();
    page.focus("#username", opts()).await.unwrap();
    page.press("Backspace").await.unwrap();
    assert_eq!(page.value("#username", opts()).await.unwrap(), "abc");
    let keys = page.text("#keys", opts()).await.unwrap();
    assert!(keys.contains("Enter;"), "{keys}");
    assert!(keys.contains("b(ctrl);"), "{keys}");
    assert!(keys.contains("Tab(shift);"), "{keys}");
    assert!(keys.contains("Backspace;"), "{keys}");
    assert!(page.press("Hyper+x").await.is_err());

    // check / uncheck / radios.
    page.check("#agree", opts()).await.unwrap();
    assert_eq!(
        page.eval("document.getElementById('agree').checked")
            .await
            .unwrap(),
        json!(true)
    );
    page.check("#agree", opts()).await.unwrap(); // idempotent
    page.uncheck("#agree", opts()).await.unwrap();
    assert_eq!(
        page.eval("document.getElementById('agree').checked")
            .await
            .unwrap(),
        json!(false)
    );
    page.check("#r2", opts()).await.unwrap();
    assert_eq!(
        page.eval("document.querySelector('input[name=r]:checked').value")
            .await
            .unwrap(),
        json!("two")
    );

    // select by value / label; multiple.
    assert_eq!(
        page.select("#color", &["green"], opts()).await.unwrap(),
        vec!["green"]
    );
    assert_eq!(page.value("#color", opts()).await.unwrap(), "green");
    page.select("#color", &["Blue"], opts()).await.unwrap();
    assert_eq!(page.value("#color", opts()).await.unwrap(), "blue");
    assert_eq!(
        page.select("#multi", &["a", "c"], opts()).await.unwrap(),
        vec!["a", "c"]
    );
    assert!(page.select("#username", &["x"], opts()).await.is_err());

    // click / dblclick / hover / submit.
    page.click("#counter", opts()).await.unwrap();
    page.click("#counter", opts()).await.unwrap();
    assert_eq!(page.text("#clicks", opts()).await.unwrap(), "2");
    page.dblclick("#dbl", opts()).await.unwrap();
    assert_eq!(page.text("#clicks", opts()).await.unwrap(), "double");
    page.hover("#hoverme", opts()).await.unwrap();
    assert_eq!(page.text("#hovered", opts()).await.unwrap(), "hovered");
    page.fill("#username", "zed", opts()).await.unwrap();
    page.click("text=Log in", opts()).await.unwrap();
    assert_eq!(
        page.text("#submitted", opts()).await.unwrap(),
        "submitted:zed"
    );

    // Element actions through all().
    let buttons = page.all("button").await.unwrap();
    let counter = buttons.iter().find(|e| e.label().ends_with("[1]")).unwrap();
    assert_eq!(counter.attr("id").await.unwrap(), Some("counter".into()));
    page.eval("document.getElementById('clicks').textContent = '0'")
        .await
        .unwrap();
    counter.click(opts()).await.unwrap();
    assert_eq!(page.text("#clicks", opts()).await.unwrap(), "1");
    let input = page.wait("#username", opts()).await.unwrap();
    input.fill("via element", opts()).await.unwrap();
    assert_eq!(input.value().await.unwrap(), "via element");
    assert!(input.is_visible().await.unwrap());

    b.close().await.unwrap();
}

#[tokio::test]
async fn auto_wait_and_timeouts() {
    let Some(b) = browser("auto_wait_and_timeouts").await else {
        return;
    };
    let fx = Fixture::start().await;
    let page = b.page(1).await.unwrap();
    page.goto(&fx.url("/delayed.html"), WaitUntil::Load)
        .await
        .unwrap();

    // Appended by a page timer 800 ms after load: it is absent right after
    // the navigation settles, and text() waits for attachment rather than
    // failing. The absence check is only meaningful when the first world
    // (createIsolatedWorld + resolver install) came up well inside those
    // 800 ms — under a parallel workspace run with a dozen Chromes it may
    // not, so the check is skipped rather than made flaky.
    let settled = Instant::now();
    let present = page.exists("#appeared").await.unwrap();
    if settled.elapsed() < Duration::from_millis(500) {
        assert!(!present, "#appeared exists before the fixture's timer");
    }
    assert_eq!(page.text("#appeared", opts()).await.unwrap(), "I appeared");

    // Disabled-then-enabled: click waits; the spinner overlay (gone at
    // 600 ms) would otherwise cover it.
    page.click("#later", opts()).await.unwrap();
    assert_eq!(page.text("#result", opts()).await.unwrap(), "clicked");

    // Moving element: click waits for a stable box.
    page.click("#mover", opts()).await.unwrap();
    assert_eq!(page.text("#result", opts()).await.unwrap(), "moved-click");

    // Waits.
    page.wait_gone("#gone", opts()).await.unwrap();
    page.wait_text("#counter", "5", opts()).await.unwrap();
    assert!(page.wait("#appeared", opts()).await.is_ok());

    // Timeouts name the action, selector and last state.
    let err = page.click("#never", quick()).await.unwrap_err();
    match &err {
        BrowserError::Timeout {
            action,
            selector,
            waited_ms,
            last_state,
        } => {
            assert_eq!(action, "click");
            assert_eq!(selector.as_deref(), Some("#never"));
            assert!(*waited_ms >= 600 && *waited_ms < 5000, "{waited_ms}");
            assert_eq!(last_state.as_deref(), Some("not found"));
        }
        other => panic!("expected Timeout, got {other}"),
    }
    assert!(err.to_string().contains("click(\"#never\")"), "{err}");
    let err = page.click("#hidden-never", quick()).await.unwrap_err();
    assert!(matches!(err, BrowserError::Timeout { .. }));
    assert!(page.wait_gone("#counter", quick()).await.is_err());
    assert!(page.wait_text("#counter", "99", quick()).await.is_err());
    let err = page.wait_url("*/elsewhere", quick()).await.unwrap_err();
    assert!(err.to_string().contains("url is"), "{err}");

    // Hidden element: visible-wait reports it.
    page.goto(&fx.url("/forms.html"), WaitUntil::Load)
        .await
        .unwrap();
    let err = page.click("#hidden", quick()).await.unwrap_err();
    match err {
        BrowserError::Timeout { last_state, .. } => {
            assert_eq!(last_state.as_deref(), Some("hidden"))
        }
        other => panic!("{other}"),
    }
    // …but readers do not need visibility (innerText of an unrendered
    // element falls back to textContent).
    assert_eq!(page.text("#hidden", opts()).await.unwrap(), "hidden text");
    assert_eq!(
        page.html("#hidden", opts()).await.unwrap(),
        "<div id=\"hidden\" style=\"display:none\">hidden text</div>"
    );

    b.close().await.unwrap();
}

#[tokio::test]
async fn text_selectors_and_scrolling() {
    let Some(b) = browser("text_selectors_and_scrolling").await else {
        return;
    };
    let fx = Fixture::start().await;
    let page = b.page(1).await.unwrap();
    page.goto(&fx.url("/text.html"), WaitUntil::Load)
        .await
        .unwrap();

    // Exact (case-insensitive) match wins over substring matches and
    // picks the deepest element; scripts are ignored.
    assert_eq!(
        page.html("text=Buy now", opts()).await.unwrap(),
        "<span>Buy now</span>"
    );
    let all = page.all("text=buy now").await.unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].attr("id").await.unwrap(), Some("buy-upper".into()));
    // Substring matches when nothing matches exactly.
    let partial = page.all("text=buy now and").await.unwrap();
    assert_eq!(partial.len(), 1);
    assert_eq!(partial[0].attr("id").await.unwrap(), Some("nav-buy".into()));
    assert_eq!(page.count("text=cheap").await.unwrap(), 1);
    assert_eq!(page.count("text=nothing here").await.unwrap(), 0);
    page.click("text=Add to cart", opts()).await.unwrap();
    assert_eq!(page.text("#out", opts()).await.unwrap(), "added");
    assert_eq!(page.text("xpath=//li[2]", opts()).await.unwrap(), "beta");
    assert_eq!(page.count("//ul/li").await.unwrap(), 3);

    // Off-screen button: click scrolls it into view.
    page.goto(&fx.url("/scroll.html"), WaitUntil::Load)
        .await
        .unwrap();
    assert_eq!(page.eval("window.scrollY").await.unwrap(), json!(0));
    page.click("#bottom", opts()).await.unwrap();
    assert_eq!(page.text("#res", opts()).await.unwrap(), "bottom clicked");
    assert!(page.eval("window.scrollY").await.unwrap().as_f64().unwrap() > 1000.0);
    page.scroll_to(0.0, 0.0).await.unwrap();
    assert_eq!(page.eval("window.scrollY").await.unwrap(), json!(0));
    page.scroll_by(0.0, 100.0).await.unwrap();
    assert_eq!(page.eval("window.scrollY").await.unwrap(), json!(100));
    page.scroll_into_view("#bottom", opts()).await.unwrap();
    assert!(page.eval("window.scrollY").await.unwrap().as_f64().unwrap() > 1000.0);

    // Screenshots: viewport vs full page (PNG magic + bigger).
    let shot = page.screenshot_png(false).await.unwrap();
    assert_eq!(&shot[..8], b"\x89PNG\r\n\x1a\n");
    let full = page.screenshot_png(true).await.unwrap();
    assert_eq!(&full[..8], b"\x89PNG\r\n\x1a\n");
    assert!(full.len() > shot.len(), "{} vs {}", full.len(), shot.len());
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("s.png");
    page.screenshot(&png, false).await.unwrap();
    assert!(png.metadata().unwrap().len() > 100);
    let pdf = dir.path().join("p.pdf");
    page.pdf(&pdf).await.unwrap();
    assert_eq!(&std::fs::read(&pdf).unwrap()[..5], b"%PDF-");

    page.set_viewport(500, 400).await.unwrap();
    assert_eq!(page.eval("innerWidth").await.unwrap(), json!(500));
    assert_eq!(page.eval("innerHeight").await.unwrap(), json!(400));

    b.close().await.unwrap();
}

#[tokio::test]
async fn dialogs_follow_policy() {
    let Some(b) = browser("dialogs_follow_policy").await else {
        return;
    };
    let fx = Fixture::start().await;
    let page = b.page(1).await.unwrap();
    page.goto(&fx.url("/dialog.html"), WaitUntil::Load)
        .await
        .unwrap();

    page.click("#alert", opts()).await.unwrap();
    assert_eq!(page.text("#result", opts()).await.unwrap(), "alerted");
    page.click("#confirm", opts()).await.unwrap();
    page.wait_text("#result", "yes", opts()).await.unwrap();
    page.on_dialog(DialogPolicy::Dismiss);
    page.click("#confirm", opts()).await.unwrap();
    page.wait_text("#result", "no", opts()).await.unwrap();
    page.on_dialog(DialogPolicy::AcceptWith("surf".into()));
    page.click("#prompt", opts()).await.unwrap();
    page.wait_text("#result", "said:surf", opts())
        .await
        .unwrap();
    page.on_dialog(DialogPolicy::Accept);
    page.click("#prompt", opts()).await.unwrap();
    page.wait_text("#result", "said:dflt", opts())
        .await
        .unwrap();

    let dialogs = page.dialogs();
    assert_eq!(dialogs.len(), 5);
    assert_eq!(dialogs[0].kind, "alert");
    assert_eq!(dialogs[0].message, "hello");
    assert_eq!(dialogs[3].kind, "prompt");
    assert_eq!(dialogs[3].default_prompt.as_deref(), Some("dflt"));

    b.close().await.unwrap();
}

#[tokio::test]
async fn world_survives_navigation_and_frames_are_separate() {
    let Some(b) = browser("world_survives_navigation_and_frames_are_separate").await else {
        return;
    };
    let fx = Fixture::start().await;
    let page = b.page(1).await.unwrap();
    page.goto(&fx.url("/forms.html"), WaitUntil::Load)
        .await
        .unwrap();
    let w1 = page.world().await.unwrap();
    assert_eq!(
        page.eval("typeof document.body").await.unwrap(),
        json!("object")
    );
    // Helpers live in the isolated world only: the main world cannot see
    // the resolver (checked through the page's own script context via a
    // DOM round-trip).
    page.eval_fn(
        "function(name) { const s = document.createElement('script'); s.textContent = `document.body.dataset.seen = String(typeof window[${JSON.stringify(name)}]);`; document.head.appendChild(s); }",
        vec![json!(w1.resolver.clone())],
    )
    .await
    .unwrap();
    assert_eq!(
        page.eval("document.body.dataset.seen").await.unwrap(),
        json!("undefined")
    );
    assert_eq!(
        page.eval(&format!("typeof globalThis[{:?}]", w1.resolver))
            .await
            .unwrap(),
        json!("function")
    );

    // Navigate by clicking (the runtime did not initiate it): the next
    // eval transparently gets a fresh world.
    page.click("#link", opts()).await.unwrap();
    page.wait_url("*/nav.html", opts()).await.unwrap();
    assert_eq!(
        page.eval("document.title").await.unwrap(),
        json!("Nav fixture")
    );
    let w2 = page.world().await.unwrap();
    assert_ne!(
        (w1.context_id, w1.name.as_str()),
        (w2.context_id, w2.name.as_str())
    );
    // A stale world handle fails with a context error; the page recovers.
    let stale = w1.eval("1").await.unwrap_err();
    assert!(surf_browser::world::context_lost(&stale), "{stale}");
    assert_eq!(page.eval("1").await.unwrap(), json!(1));

    // Two frames: the main-frame world sees the frames but reads the top
    // document; element lookups stay in the main frame.
    page.goto(&fx.url("/frames.html"), WaitUntil::Load)
        .await
        .unwrap();
    assert_eq!(page.count("iframe").await.unwrap(), 2);
    assert_eq!(page.text("#top", opts()).await.unwrap(), "Top frame");
    assert!(!page.exists("#child").await.unwrap());
    assert_eq!(
        page.eval("Array.from(document.querySelectorAll('iframe')).map(f => f.name)")
            .await
            .unwrap(),
        json!(["first", "second"])
    );

    b.close().await.unwrap();
}

/// Frames API: child frames get their own isolated worlds, actions and
/// readers resolve inside the frame, `frame(FrameRef)` waits for the
/// iframe and the world is re-created after the frame navigates.
#[tokio::test]
async fn child_frames_have_their_own_worlds() {
    let Some(b) = browser("child_frames_have_their_own_worlds").await else {
        return;
    };
    let fx = Fixture::start().await;
    let page = b.page(1).await.unwrap();
    page.goto(&fx.url("/frames.html"), WaitUntil::Load)
        .await
        .unwrap();

    let main = page.main_frame();
    assert!(main.is_main());
    assert_eq!(main.text("#top", opts()).await.unwrap(), "Top frame");
    assert!(!main.exists("#child").await.unwrap());

    let frames = page.frames().await.unwrap();
    assert_eq!(frames.len(), 3, "{frames:?}");
    assert!(frames[0].is_main());
    assert_eq!(frames[1].name(), "first");
    assert_eq!(frames[2].name(), "second");
    assert_eq!(frames[1].info().parent_id.as_deref(), Some(main.frame_id()));

    let first = page.frame(FrameRef::Name("first".into())).await.unwrap();
    let second = page.frame(FrameRef::UrlGlob("*?n=2".into())).await.unwrap();
    let by_index = page.frame(FrameRef::Index(1)).await.unwrap();
    assert_eq!(second.frame_id(), by_index.frame_id());
    assert_ne!(first.frame_id(), second.frame_id());
    assert_eq!(first.text("#child", opts()).await.unwrap(), "child ?n=1");
    assert_eq!(second.text("#child", opts()).await.unwrap(), "child ?n=2");
    assert!(first
        .url()
        .await
        .unwrap()
        .ends_with("/frame-child.html?n=1"));
    assert_eq!(first.eval("document.title").await.unwrap(), json!("Child"));
    // Frame worlds are separate from the main world and from each other.
    let wm = main.world().await.unwrap();
    let w1 = first.world().await.unwrap();
    let w2 = second.world().await.unwrap();
    assert_ne!(wm.context_id, w1.context_id);
    assert_ne!(w1.context_id, w2.context_id);
    assert_eq!(first.world().await.unwrap().context_id, w1.context_id);

    // Element handles stay bound to their frame.
    let el = first.first("#child").await.unwrap().expect("child");
    assert_eq!(el.frame().frame_id(), first.frame_id());
    assert_eq!(el.text().await.unwrap(), "child ?n=1");

    // Navigate the child: its world is re-created, the main frame's stays.
    first
        .eval("location.href = '/frame-child.html?n=9'")
        .await
        .ok();
    first
        .wait_text("#child", "child ?n=9", opts())
        .await
        .unwrap();
    let w1b = first.world().await.unwrap();
    assert_ne!(w1.context_id, w1b.context_id);
    assert_eq!(main.world().await.unwrap().context_id, wm.context_id);

    // A missing frame times out with the frame list.
    let err = page.frame(FrameRef::Name("nope".into())).await.unwrap_err();
    assert!(err.to_string().contains("first"), "{err}");

    b.close().await.unwrap();
}

/// Shadow DOM piercing (CSS and `text=`), `fill()` on special inputs and
/// `set_files()` on a file input.
#[tokio::test]
async fn shadow_dom_special_fill_and_set_files() {
    let Some(b) = browser("shadow_dom_special_fill_and_set_files").await else {
        return;
    };
    let fx = Fixture::start().await;
    let page = b.page(1).await.unwrap();
    page.goto(&fx.url("/shadow.html"), WaitUntil::Load)
        .await
        .unwrap();

    // CSS pierces open shadow roots; light DOM wins; one selector never
    // crosses a boundary; closed roots are invisible.
    assert_eq!(page.count(".inner").await.unwrap(), 3);
    assert!(page.exists("#shadow-btn").await.unwrap());
    assert!(!page.exists("#host .inner").await.unwrap());
    assert!(!page.exists(".secret").await.unwrap());
    assert_eq!(page.text("i.inner", opts()).await.unwrap(), "nested text");
    page.click("#shadow-btn", opts()).await.unwrap();
    assert_eq!(page.text("#light", opts()).await.unwrap(), "shadow clicked");
    // text= walks the composed tree: shadow text, nested roots and slotted
    // light children each found once.
    assert_eq!(
        page.text("text=shadow text", opts()).await.unwrap(),
        "shadow text"
    );
    assert_eq!(
        page.text("text=nested text", opts()).await.unwrap(),
        "nested text"
    );
    assert_eq!(page.count("text=slotted text").await.unwrap(), 1);
    assert_eq!(page.count("text=closed text").await.unwrap(), 0);

    // Special inputs get `.value` + events; invalid values are errors.
    page.fill("#date", "2024-02-29", opts()).await.unwrap();
    assert_eq!(page.value("#date", opts()).await.unwrap(), "2024-02-29");
    page.fill("#number", "7", opts()).await.unwrap();
    assert_eq!(page.value("#number", opts()).await.unwrap(), "7");
    page.fill("#color", "#ff0000", opts()).await.unwrap();
    assert_eq!(page.value("#color", opts()).await.unwrap(), "#ff0000");
    let err = page.fill("#date", "not a date", opts()).await.unwrap_err();
    assert!(err.to_string().contains("not a valid value"), "{err}");
    let err = page.fill("#file", "x.txt", opts()).await.unwrap_err();
    assert!(err.to_string().contains("set_files"), "{err}");

    // set_files: paths must exist; the input fires `change`; an empty
    // list clears; a non-file input is refused.
    let dir = std::env::temp_dir().join(format!("surf-set-files-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.txt");
    let bfile = dir.join("b.bin");
    std::fs::write(&a, b"hello").unwrap();
    std::fs::write(&bfile, [0u8; 3]).unwrap();
    page.set_files("#file", &[&a, &bfile], opts())
        .await
        .unwrap();
    assert_eq!(
        page.text("#files", opts()).await.unwrap(),
        "a.txt:5,b.bin:3"
    );
    assert_eq!(
        page.eval("document.getElementById('file').files.length")
            .await
            .unwrap(),
        json!(2)
    );
    let el = page.first("#file").await.unwrap().expect("file input");
    el.set_files(&[], opts()).await.unwrap();
    assert_eq!(
        page.eval("document.getElementById('file').files.length")
            .await
            .unwrap(),
        json!(0)
    );
    let missing = dir.join("missing.txt");
    let err = page
        .set_files("#file", &[&missing], opts())
        .await
        .unwrap_err();
    assert!(err.to_string().contains("missing.txt"), "{err}");
    let err = page.set_files("#date", &[&a], opts()).await.unwrap_err();
    assert!(err.to_string().contains("file"), "{err}");
    std::fs::remove_dir_all(&dir).ok();

    b.close().await.unwrap();
}

/// Decision 9: `Runtime.addBinding{executionContextId}` on the isolated
/// world raises `Runtime.bindingCalled` without `Runtime.enable` — the
/// channel `on element_appears` (task 8) is built on.
#[tokio::test]
async fn binding_called_fires_without_runtime_enable() {
    let Some(b) = browser("binding_called_fires_without_runtime_enable").await else {
        return;
    };
    let fx = Fixture::start().await;
    let page = b.page(1).await.unwrap();
    page.goto(&fx.url("/forms.html"), WaitUntil::Load)
        .await
        .unwrap();
    let world = page.world().await.unwrap();
    let mut events = world.session.events("Runtime.bindingCalled");
    let name = format!("__surf_bind_{}", surf_browser::util::random_ident(8));
    world.add_binding(&name).await.unwrap();
    // Main world cannot see the binding; the isolated world can call it.
    page.eval_fn(
        "function(n) { const s = document.createElement('script'); s.textContent = `document.body.dataset.bind = typeof window[${JSON.stringify(n)}];`; document.head.appendChild(s); }",
        vec![json!(name.clone())],
    )
    .await
    .unwrap();
    assert_eq!(
        page.eval("document.body.dataset.bind").await.unwrap(),
        json!("undefined")
    );
    world
        .call(
            "function(n) { globalThis[n](JSON.stringify({hello: 'world'})); }",
            vec![json!(name.clone())],
        )
        .await
        .unwrap();
    let ev = tokio::time::timeout(Duration::from_secs(5), surf_cdp::event::next(&mut events))
        .await
        .expect("bindingCalled within 5 s")
        .expect("channel open");
    assert_eq!(ev.params["name"], json!(name));
    assert_eq!(ev.params["payload"], json!("{\"hello\":\"world\"}"));
    assert_eq!(ev.params["executionContextId"], json!(world.context_id));
    b.close().await.unwrap();
}

#[tokio::test]
async fn page_registry_names_and_ambiguity() {
    let Some(b) = browser("page_registry_names_and_ambiguity").await else {
        return;
    };
    let fx = Fixture::start().await;
    assert!(b.pages().is_empty());
    // sole_page auto-creates page 1.
    let p1 = b.sole_page().await.unwrap();
    assert_eq!(p1.index(), 1);
    let again = b.sole_page().await.unwrap();
    assert_eq!(again.index(), 1);
    // page(3) creates 2 and 3.
    let p3 = b.page(3).await.unwrap();
    assert_eq!(p3.index(), 3);
    assert_eq!(b.pages().len(), 3);
    // page("login") creates 4 and names it; same name → same page.
    let login = b.page_named("login").await.unwrap();
    assert_eq!(login.index(), 4);
    assert_eq!(login.name().as_deref(), Some("login"));
    assert_eq!(b.page_named("login").await.unwrap().index(), 4);
    assert_eq!(b.pages().len(), 4);

    let err = b.sole_page().await.unwrap_err();
    match &err {
        BrowserError::Ambiguous { names } => {
            assert_eq!(names, &["1", "2", "3", "\"login\""]);
        }
        other => panic!("expected Ambiguous, got {other}"),
    }
    assert!(
        err.to_string()
            .contains("several pages are open (1, 2, 3, \"login\") — say which: page(2)."),
        "{err}"
    );

    // Pages are independent tabs.
    p1.goto(&fx.url("/forms.html"), WaitUntil::Load)
        .await
        .unwrap();
    login
        .goto(&fx.url("/nav.html"), WaitUntil::Load)
        .await
        .unwrap();
    assert_eq!(p1.title().await.unwrap(), "Forms fixture");
    assert_eq!(login.title().await.unwrap(), "Nav fixture");
    assert_eq!(p3.url().await.unwrap(), "about:blank");

    // Closing drops it from the registry; closed pages refuse actions.
    b.close_page(&p3).await.unwrap();
    assert_eq!(b.pages().len(), 3);
    assert!(!p3.is_open());
    assert!(matches!(
        p3.title().await.unwrap_err(),
        BrowserError::PageClosed { index: 3 }
    ));
    assert!(matches!(p3.close().await, Ok(())));

    b.close().await.unwrap();
    assert!(b.close().await.is_ok(), "close is idempotent");
}

#[tokio::test]
async fn browser_contexts_isolate_cookies_and_rebind_migrates() {
    let Some(b) = browser("browser_contexts_isolate_cookies_and_rebind_migrates").await else {
        return;
    };
    let fx = Fixture::start().await;
    let a = b.page(1).await.unwrap();
    let shared = b.page(2).await.unwrap();
    let isolated = b
        .new_page(NewPageOptions {
            isolated: true,
            name: Some("iso".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(isolated.backing().unwrap().browser_context_id.is_some());
    assert!(a.backing().unwrap().browser_context_id.is_none());

    a.goto(&fx.url("/set-cookie?who=alice"), WaitUntil::Load)
        .await
        .unwrap();
    a.goto(&fx.url("/cookie-echo"), WaitUntil::Load)
        .await
        .unwrap();
    assert_eq!(a.text("body", opts()).await.unwrap(), "who=alice");
    // Same (default) context sees it; the isolated context does not.
    shared
        .goto(&fx.url("/cookie-echo"), WaitUntil::Load)
        .await
        .unwrap();
    assert_eq!(shared.text("body", opts()).await.unwrap(), "who=alice");
    isolated
        .goto(&fx.url("/cookie-echo"), WaitUntil::Load)
        .await
        .unwrap();
    assert_eq!(isolated.text("body", opts()).await.unwrap(), "");

    // cookies() / set_cookies() / clear_cookies() are context-scoped.
    let jar = a.cookies().await.unwrap();
    assert_eq!(jar.len(), 1);
    assert_eq!(
        (jar[0].name.as_str(), jar[0].value.as_str()),
        ("who", "alice")
    );
    assert_eq!(jar[0].domain.as_deref(), Some("127.0.0.1"));
    assert!(isolated.cookies().await.unwrap().is_empty());
    isolated
        .set_cookies(&[Cookie::for_url("who", "iso", &fx.base)])
        .await
        .unwrap();
    isolated.reload(WaitUntil::Load).await.unwrap();
    assert_eq!(isolated.text("body", opts()).await.unwrap(), "who=iso");
    assert_eq!(a.cookies().await.unwrap()[0].value, "alice");
    isolated.clear_cookies().await.unwrap();
    assert!(isolated.cookies().await.unwrap().is_empty());
    assert_eq!(a.cookies().await.unwrap().len(), 1);

    // Rebind the isolated page into a brand-new context: cookies, storage
    // and URL come along; the handle, index and name stay.
    isolated
        .set_cookies(&[Cookie::for_url("who", "iso2", &fx.base)])
        .await
        .unwrap();
    isolated
        .goto(&fx.url("/nav.html"), WaitUntil::Load)
        .await
        .unwrap();
    isolated
        .eval("localStorage.setItem('k', 'v1'); sessionStorage.setItem('s', 'v2'); 0")
        .await
        .unwrap();
    let old_backing = isolated.backing().unwrap();
    let old_ctx = old_backing.browser_context_id.clone().unwrap();
    let fresh = b
        .create_backing(Some(
            b.root()
                .send(surf_cdp::protocol::target::CreateBrowserContext::default())
                .await
                .unwrap()
                .browser_context_id,
        ))
        .await
        .unwrap();
    assert_ne!(fresh.browser_context_id, old_backing.browser_context_id);
    isolated
        .rebind(fresh.clone(), Migration::ALL)
        .await
        .unwrap();
    assert_eq!(isolated.index(), 3);
    assert_eq!(isolated.name().as_deref(), Some("iso"));
    assert_eq!(isolated.backing().unwrap().target_id, fresh.target_id);
    assert_eq!(isolated.url().await.unwrap(), fx.url("/nav.html"));
    assert_eq!(
        isolated.eval("localStorage.getItem('k')").await.unwrap(),
        json!("v1")
    );
    assert_eq!(
        isolated.eval("sessionStorage.getItem('s')").await.unwrap(),
        json!("v2")
    );
    let jar = isolated.cookies().await.unwrap();
    assert_eq!(jar.len(), 1);
    assert_eq!(jar[0].value, "iso2");
    isolated
        .goto(&fx.url("/cookie-echo"), WaitUntil::Load)
        .await
        .unwrap();
    assert_eq!(isolated.text("body", opts()).await.unwrap(), "who=iso2");
    // The old target is gone.
    let targets = b
        .root()
        .send(surf_cdp::protocol::target::GetTargets::default())
        .await
        .unwrap();
    assert!(
        !targets
            .target_infos
            .iter()
            .any(|t| t.target_id == old_backing.target_id),
        "old target should be closed"
    );
    let _ = old_ctx;

    // Browser::rebind_page keeps the same context when no proxy is given.
    let before = a.backing().unwrap();
    b.rebind_page(&a, None, Migration::ALL).await.unwrap();
    let after = a.backing().unwrap();
    assert_ne!(before.target_id, after.target_id);
    assert_eq!(after.browser_context_id, None);
    assert_eq!(a.url().await.unwrap(), fx.url("/cookie-echo"));
    assert_eq!(a.text("body", opts()).await.unwrap(), "who=alice");

    b.close().await.unwrap();
}

/// `rebind_page_to`: `FreshContext` lands in a new, empty context (cookies
/// gone) and keeps the page's proxy; `Proxy` changes it; the handle, index
/// and name survive. Closed indices stay taken (`page(n)` for one is an
/// error, `page(n+1)` still auto-creates).
#[tokio::test]
async fn rebind_targets_and_closed_indices() {
    let Some(b) = browser("rebind_targets_and_closed_indices").await else {
        return;
    };
    let fx = Fixture::start().await;
    let proxy_a = surf_testserver::Proxy::start("A").await;
    let proxy_b = surf_testserver::Proxy::start("B").await;
    let page = b
        .new_page(NewPageOptions {
            name: Some("job".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(page.index(), 1);
    page.goto(&fx.url("/set-cookie?who=alice"), WaitUntil::Load)
        .await
        .unwrap();
    assert_eq!(page.cookies().await.unwrap().len(), 1);
    assert_eq!(b.page_proxy(&page), None);

    // Fresh context: no cookies, same handle / index / name, no proxy yet.
    b.rebind_page_to(&page, RebindTarget::FreshContext, Migration::default())
        .await
        .unwrap();
    assert!(page.is_open());
    assert_eq!(page.index(), 1);
    assert_eq!(page.name().as_deref(), Some("job"));
    assert!(page.backing().unwrap().browser_context_id.is_some());
    assert!(page.cookies().await.unwrap().is_empty());
    assert_eq!(b.page_proxy(&page), None);

    // Proxy: a new context behind A (loopback hosts bypass proxies, so the
    // registry is what we check), then FreshContext keeps A, Proxy(B) swaps.
    b.rebind_page_to(
        &page,
        RebindTarget::Proxy(proxy_a.url.clone()),
        Migration::default(),
    )
    .await
    .unwrap();
    assert_eq!(b.page_proxy(&page).as_deref(), Some(proxy_a.url.as_str()));
    let ctx_a = page.backing().unwrap().browser_context_id;
    b.rebind_page_to(&page, RebindTarget::FreshContext, Migration::default())
        .await
        .unwrap();
    assert_eq!(b.page_proxy(&page).as_deref(), Some(proxy_a.url.as_str()));
    assert_ne!(page.backing().unwrap().browser_context_id, ctx_a);
    b.rebind_page_to(
        &page,
        RebindTarget::Proxy(proxy_b.url.clone()),
        Migration::default(),
    )
    .await
    .unwrap();
    assert_eq!(b.page_proxy(&page).as_deref(), Some(proxy_b.url.as_str()));
    // Same context keeps the proxy and the context.
    let ctx_b = page.backing().unwrap().browser_context_id;
    b.rebind_page_to(&page, RebindTarget::SameContext, Migration::default())
        .await
        .unwrap();
    assert_eq!(page.backing().unwrap().browser_context_id, ctx_b);
    assert_eq!(b.page_proxy(&page).as_deref(), Some(proxy_b.url.as_str()));
    page.goto(&fx.url("/"), WaitUntil::Load).await.unwrap();
    assert_eq!(page.title().await.unwrap(), "Index");

    // Closed indices do not shift and are not re-created.
    let second = b.page(2).await.unwrap();
    b.close_page(&page).await.unwrap();
    assert_eq!(b.page_proxy(&page), None);
    assert!(matches!(
        b.page(1).await,
        Err(BrowserError::PageClosed { index: 1 })
    ));
    assert_eq!(b.page(2).await.unwrap().index(), second.index());
    assert_eq!(b.page(3).await.unwrap().index(), 3);
    assert_eq!(b.pages().len(), 2);

    b.close().await.unwrap();
}

/// `io::Write` into a shared buffer for the tracing subscriber.
#[derive(Clone, Default)]
struct Buf(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Buf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The quiet contract, measured: trace every frame of a full session and
/// assert `Page.enable` is the only `*.enable`, and that `Runtime.enable`
/// / `DOM.enable` / `DOM.getDocument` never go out. Fetch / Network are
/// not enabled either because no proxy auth or hook is involved.
#[test]
fn quiet_contract_only_page_enable_is_sent() {
    let buf = Buf::default();
    let sink = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || sink.clone())
        .finish();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let ran = tracing::subscriber::with_default(subscriber, || {
        rt.block_on(async {
            let Some(b) = browser("quiet_contract_only_page_enable_is_sent").await else {
                return false;
            };
            b.connection().set_trace(true);
            let fx = Fixture::start().await;
            let page = b.page(2).await.unwrap();
            page.goto(&fx.url("/forms.html"), WaitUntil::Load)
                .await
                .unwrap();
            page.type_text("#username", "q", opts()).await.unwrap();
            page.click("#counter", opts()).await.unwrap();
            page.hover("#hoverme", opts()).await.unwrap();
            page.select("#color", &["red"], opts()).await.unwrap();
            let _ = page.text("#para", opts()).await.unwrap();
            let _ = page.all("a").await.unwrap();
            let _ = page.count("text=Log in").await.unwrap();
            let _ = page.screenshot_png(false).await.unwrap();
            let _ = page.cookies().await.unwrap();
            page.set_viewport(800, 600).await.unwrap();
            page.wait_text("#clicks", "1", opts()).await.unwrap();
            page.click("#link", opts()).await.unwrap();
            page.wait_url("*/nav.html", opts()).await.unwrap();
            page.back(WaitUntil::Load).await.unwrap();
            b.close().await.unwrap();
            true
        })
    });
    drop(rt);
    if !ran {
        return;
    }
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    let re = regex::Regex::new(r#""method":"([A-Za-z.]+)""#).unwrap();
    let sent: Vec<String> = text
        .lines()
        .filter(|l| l.contains("surf_cdp::trace") && l.contains('→'))
        .filter_map(|l| re.captures(l).map(|c| c[1].to_owned()))
        .collect();
    assert!(sent.len() > 20, "trace captured only {} frames", sent.len());
    let enables: std::collections::BTreeSet<&str> = sent
        .iter()
        .map(String::as_str)
        .filter(|m| m.ends_with(".enable"))
        .collect();
    assert_eq!(
        enables.into_iter().collect::<Vec<_>>(),
        vec!["Page.enable"],
        "only Page.enable may be sent"
    );
    for forbidden in [
        "Runtime.enable",
        "DOM.enable",
        "DOM.getDocument",
        "Network.enable",
        "Fetch.enable",
    ] {
        assert!(!sent.iter().any(|m| m == forbidden), "{forbidden} was sent");
    }
    let domains: std::collections::BTreeSet<&str> =
        sent.iter().filter_map(|m| m.split('.').next()).collect();
    eprintln!(
        "quiet contract: {} frames, domains {:?}",
        sent.len(),
        domains
    );
}
