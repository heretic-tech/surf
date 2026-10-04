//! Auto-waiting page actions, readers and waits — methods on [`Page`] and
//! [`Element`].
//!
//! Every action resolves its element through the isolated-world resolver
//! and waits until it is **attached → visible → stable → enabled**,
//! polling every [`POLL_INTERVAL`] up to the page timeout (or
//! `ActionOptions::timeout`), then `DOM.scrollIntoViewIfNeeded{objectId}`
//! → `DOM.getContentQuads{objectId}` → centre of the first quad →
//! `Input.dispatchMouseEvent` (`mouseMoved`, `mousePressed`,
//! `mouseReleased`). A point that is covered by another element (overlay,
//! spinner) is retried too. On timeout the error names the action, the
//! selector, how long was waited and what the element looked like last
//! (`not found`, `hidden`, `moving`, `disabled`, `covered`).
//!
//! Readers (`text`, `html`, `attr`, `value`) wait for the element to be
//! attached only. `exists` / `count` never wait. Nothing here enables
//! `DOM`; `DOM.scrollIntoViewIfNeeded` and `DOM.getContentQuads` work on
//! an `objectId` without it.

use crate::error::BrowserError;
use crate::input::{self, KeyPress, MouseButton};
use crate::network::UrlPattern;
use crate::page::Page;
use crate::selector::Selector;
use crate::world::{by_value, context_lost, World};
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use surf_cdp::protocol::dom;

/// How often an auto-wait re-checks its element.
pub const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Per-call options shared by most actions.
#[derive(Debug, Clone, Default)]
pub struct ActionOptions {
    /// Override the page timeout.
    pub timeout: Option<Duration>,
    /// `type`: delay between keys (±50 % jitter). Default: none.
    pub delay: Option<Duration>,
}

impl ActionOptions {
    /// With this timeout.
    pub fn timeout(d: Duration) -> ActionOptions {
        ActionOptions {
            timeout: Some(d),
            ..Default::default()
        }
    }
}

/// A resolved element handle (`objectId` in the page's isolated world),
/// as returned by [`Page::all`]. Offers the same actions and readers as
/// [`Page`] without re-resolving. Valid until the page navigates.
pub struct Element {
    page: Page,
    world: World,
    object_id: String,
    /// `selector[index]` for error messages.
    label: String,
}

impl std::fmt::Debug for Element {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Element")
            .field("label", &self.label)
            .field("object_id", &self.object_id)
            .finish()
    }
}

impl Drop for Element {
    fn drop(&mut self) {
        let world = self.world.clone();
        let id = std::mem::take(&mut self.object_id);
        if world.session.connection().is_closed() {
            return;
        }
        if let Ok(h) = tokio::runtime::Handle::try_current() {
            h.spawn(async move { world.release(&id).await });
        }
    }
}

/// What an action acts on.
#[derive(Clone)]
enum Target {
    Selector(Selector),
    Handle { object_id: String, label: String },
}

impl Target {
    fn label(&self) -> String {
        match self {
            Target::Selector(s) => s.source(),
            Target::Handle { label, .. } => label.clone(),
        }
    }
}

/// Readiness report from [`READY_CHECK`].
struct Ready {
    state: String,
}

/// Runs with `this` = element: attached? visible? disabled? stable over
/// two animation frames (with a 50 ms fallback for throttled tabs)?
const READY_CHECK: &str = r#"async function() {
  const el = this;
  if (!el.isConnected) return { state: 'detached' };
  const target = el.localName === 'option' ? (el.closest('select') || el) : el;
  const rectOf = (e) => { const r = e.getBoundingClientRect(); return [r.x, r.y, r.width, r.height]; };
  const style = getComputedStyle(target);
  const r1 = rectOf(target);
  const visible = r1[2] > 0 && r1[3] > 0 && style.visibility !== 'hidden' && style.display !== 'none';
  if (!visible) return { state: 'hidden' };
  await new Promise((resolve) => {
    let done = false;
    const finish = () => { if (!done) { done = true; resolve(); } };
    requestAnimationFrame(() => requestAnimationFrame(finish));
    setTimeout(finish, 50);
  });
  if (!el.isConnected) return { state: 'detached' };
  const r2 = rectOf(target);
  if (!r1.every((v, i) => Math.abs(v - r2[i]) < 0.5)) return { state: 'moving' };
  const disabled = el.matches(':disabled') || el.getAttribute('aria-disabled') === 'true';
  if (disabled) return { state: 'disabled' };
  return { state: 'ready' };
}"#;

/// `this` = element, `(x, y)` = intended click point: is the element (or
/// a descendant / ancestor) what `elementFromPoint` returns there?
const HIT_CHECK: &str = r#"function(x, y) {
  const hit = document.elementFromPoint(x, y);
  if (!hit) return 'ready';
  if (hit === this || this.contains(hit) || hit.contains(this)) return 'ready';
  const label = hit.localName + (hit.id ? '#' + hit.id : '');
  return 'covered by <' + label + '>';
}"#;

const TEXT_OF: &str = r#"function() {
  if (this instanceof HTMLElement) return this.innerText;
  return this.textContent || '';
}"#;

const FOCUS: &str = "function() { this.focus(); }";

const SELECT_ALL: &str = r#"function() {
  this.focus();
  if (typeof this.select === 'function' && !(this instanceof HTMLSelectElement)) { this.select(); return; }
  const sel = window.getSelection();
  sel.removeAllRanges();
  const range = document.createRange();
  range.selectNodeContents(this);
  sel.addRange(range);
}"#;

const SELECT_OPTIONS: &str = r#"function(values) {
  if (!(this instanceof HTMLSelectElement)) throw new Error('select(): element is <' + this.localName + '>, not <select>');
  const wanted = new Set(values);
  const chosen = [];
  for (const o of this.options) {
    const hit = wanted.has(o.value) || wanted.has(o.label) || wanted.has(o.text.trim());
    o.selected = hit;
    if (hit) chosen.push(o.value);
    if (hit && !this.multiple) break;
  }
  this.dispatchEvent(new Event('input', { bubbles: true }));
  this.dispatchEvent(new Event('change', { bubbles: true }));
  return chosen;
}"#;

impl Page {
    // ───────────────────────── internals ─────────────────────────

    fn deadline(&self, opts: &ActionOptions) -> (Instant, Duration) {
        let t = opts.timeout.unwrap_or_else(|| self.timeout());
        (Instant::now() + t, t)
    }

    fn timeout_error(
        &self,
        action: &str,
        target: &Target,
        started: Instant,
        last_state: &str,
    ) -> BrowserError {
        BrowserError::Timeout {
            action: action.into(),
            selector: Some(target.label()),
            waited_ms: started.elapsed().as_millis() as u64,
            last_state: Some(last_state.to_owned()),
        }
    }

    /// The target's handle in `world`, if attached right now.
    async fn locate(&self, world: &World, target: &Target) -> Result<Option<String>, BrowserError> {
        match target {
            Target::Selector(sel) => Ok(world.resolve(sel, false).await?.into_iter().next()),
            Target::Handle { object_id, .. } => Ok(Some(object_id.clone())),
        }
    }

    /// Wait until the target is attached; returns `(world, objectId)`.
    async fn wait_attached(
        &self,
        action: &str,
        target: &Target,
        opts: &ActionOptions,
    ) -> Result<(World, String), BrowserError> {
        let started = Instant::now();
        let (deadline, _) = self.deadline(opts);
        loop {
            let world = self.world().await?;
            match self.locate(&world, target).await {
                Ok(Some(id)) => return Ok((world, id)),
                Ok(None) => {}
                Err(e) if context_lost(&e) => self.invalidate_world(),
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Err(self.timeout_error(action, target, started, "not found"));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Wait until the target is attached, visible, stable and enabled,
    /// scroll it into view and return `(world, objectId, x, y)` with the
    /// click point.
    async fn wait_ready(
        &self,
        action: &str,
        target: &Target,
        opts: &ActionOptions,
    ) -> Result<(World, String, f64, f64), BrowserError> {
        let started = Instant::now();
        let (deadline, _) = self.deadline(opts);
        // `wait_navigation()` after this action must notice a navigation
        // that commits before it is called.
        self.mark_action();
        loop {
            let world = self.world().await?;
            let step = async {
                let Some(id) = self.locate(&world, target).await? else {
                    return Ok::<_, BrowserError>(Err("not found".to_owned()));
                };
                let r = world.call_on(&id, READY_CHECK, vec![]).await?;
                let ready = Ready {
                    state: r["state"].as_str().unwrap_or("unknown").to_owned(),
                };
                if ready.state != "ready" {
                    self.release_if_resolved(&world, target, &id).await;
                    return Ok(Err(ready.state));
                }
                let session = &world.session;
                let scrolled = session
                    .send(dom::ScrollIntoViewIfNeeded {
                        object_id: Some(id.clone()),
                        ..Default::default()
                    })
                    .await;
                if let Err(e) = scrolled {
                    self.release_if_resolved(&world, target, &id).await;
                    return Ok(Err(format!("no layout ({})", short(&e))));
                }
                let quads = session
                    .send(dom::GetContentQuads {
                        object_id: Some(id.clone()),
                        ..Default::default()
                    })
                    .await;
                let point = match quads {
                    Ok(q) => q.quads.iter().find_map(|quad| quad_centre(quad)),
                    Err(e) => {
                        self.release_if_resolved(&world, target, &id).await;
                        return Ok(Err(format!("no layout ({})", short(&e))));
                    }
                };
                let Some((x, y)) = point else {
                    self.release_if_resolved(&world, target, &id).await;
                    return Ok(Err("hidden".to_owned()));
                };
                let hit = world
                    .call_on(&id, HIT_CHECK, vec![json!(x), json!(y)])
                    .await?;
                if hit != "ready" {
                    self.release_if_resolved(&world, target, &id).await;
                    return Ok(Err(hit.as_str().unwrap_or("covered").to_owned()));
                }
                Ok(Ok((id, x, y)))
            };
            let last_state = match step.await {
                Ok(Ok((id, x, y))) => return Ok((world, id, x, y)),
                Ok(Err(state)) => state,
                Err(e) if context_lost(&e) => {
                    self.invalidate_world();
                    if let Target::Handle { .. } = target {
                        return Err(BrowserError::Element {
                            message: "element belongs to a document that navigated away".into(),
                            selector: target.label(),
                        });
                    }
                    "page navigated".to_owned()
                }
                Err(e) => return Err(e),
            };
            if Instant::now() >= deadline {
                return Err(self.timeout_error(action, target, started, &last_state));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    async fn release_if_resolved(&self, world: &World, target: &Target, id: &str) {
        if let Target::Selector(_) = target {
            world.release(id).await;
        }
    }

    async fn click_target(
        &self,
        action: &str,
        target: &Target,
        button: MouseButton,
        count: i64,
        opts: &ActionOptions,
    ) -> Result<(), BrowserError> {
        let (world, id, x, y) = self.wait_ready(action, target, opts).await?;
        let r = input::click_at(&world.session, x, y, button, count).await;
        self.release_if_resolved(&world, target, &id).await;
        r
    }

    async fn hover_target(
        &self,
        target: &Target,
        opts: &ActionOptions,
    ) -> Result<(), BrowserError> {
        let (world, id, x, y) = self.wait_ready("hover", target, opts).await?;
        let r = input::mouse_move(&world.session, x, y).await;
        self.release_if_resolved(&world, target, &id).await;
        r
    }

    async fn focus_target(
        &self,
        action: &str,
        target: &Target,
        opts: &ActionOptions,
    ) -> Result<(World, String), BrowserError> {
        let (world, id, _, _) = self.wait_ready(action, target, opts).await?;
        world.call_on(&id, FOCUS, vec![]).await?;
        Ok((world, id))
    }

    async fn type_target(
        &self,
        target: &Target,
        text: &str,
        opts: &ActionOptions,
    ) -> Result<(), BrowserError> {
        let (world, id) = self.focus_target("type", target, opts).await?;
        let r = type_keys(&world, text, opts.delay).await;
        self.release_if_resolved(&world, target, &id).await;
        r
    }

    async fn fill_target(
        &self,
        target: &Target,
        text: &str,
        opts: &ActionOptions,
    ) -> Result<(), BrowserError> {
        let (world, id, _, _) = self.wait_ready("fill", target, opts).await?;
        let r = async {
            world.call_on(&id, SELECT_ALL, vec![]).await?;
            if text.is_empty() {
                let del = KeyPress {
                    key: input::lookup("Delete").expect("Delete key"),
                    modifiers: 0,
                };
                input::press_key(&world.session, &del).await
            } else {
                input::insert_text(&world.session, text).await
            }
        }
        .await;
        self.release_if_resolved(&world, target, &id).await;
        r
    }

    async fn press_target(
        &self,
        target: &Target,
        key: &str,
        opts: &ActionOptions,
    ) -> Result<(), BrowserError> {
        let press = input::parse_combo(key)?;
        let (world, id) = self.focus_target("press", target, opts).await?;
        let r = input::press_key(&world.session, &press).await;
        self.release_if_resolved(&world, target, &id).await;
        r
    }

    async fn set_checked_target(
        &self,
        target: &Target,
        checked: bool,
        opts: &ActionOptions,
    ) -> Result<(), BrowserError> {
        let action = if checked { "check" } else { "uncheck" };
        let (world, id, x, y) = self.wait_ready(action, target, opts).await?;
        let r = async {
            let now = world
                .call_on(&id, "function() { return !!this.checked; }", vec![])
                .await?;
            if now.as_bool() == Some(checked) {
                return Ok(());
            }
            input::click_at(&world.session, x, y, MouseButton::Left, 1).await?;
            let after = world
                .call_on(&id, "function() { return !!this.checked; }", vec![])
                .await?;
            if after.as_bool() != Some(checked) {
                return Err(BrowserError::Element {
                    message: format!("{action}: clicking did not change the checked state"),
                    selector: target.label(),
                });
            }
            Ok(())
        }
        .await;
        self.release_if_resolved(&world, target, &id).await;
        r
    }

    async fn select_target(
        &self,
        target: &Target,
        values: &[&str],
        opts: &ActionOptions,
    ) -> Result<Vec<String>, BrowserError> {
        let (world, id, _, _) = self.wait_ready("select", target, opts).await?;
        let r = world
            .call_on(&id, SELECT_OPTIONS, vec![json!(values)])
            .await
            .map(|v| {
                v.as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().map(str::to_owned))
                            .collect()
                    })
                    .unwrap_or_default()
            });
        self.release_if_resolved(&world, target, &id).await;
        r
    }

    async fn scroll_target(
        &self,
        target: &Target,
        opts: &ActionOptions,
    ) -> Result<(), BrowserError> {
        let (world, id) = self.wait_attached("scroll", target, opts).await?;
        let r = world
            .session
            .send(dom::ScrollIntoViewIfNeeded {
                object_id: Some(id.clone()),
                ..Default::default()
            })
            .await
            .map(|_| ())
            .map_err(BrowserError::from);
        self.release_if_resolved(&world, target, &id).await;
        r
    }

    async fn read_target(
        &self,
        action: &str,
        target: &Target,
        decl: &str,
        args: Vec<Value>,
        opts: &ActionOptions,
    ) -> Result<Value, BrowserError> {
        let (world, id) = self.wait_attached(action, target, opts).await?;
        let r = world.call_on(&id, decl, args).await;
        self.release_if_resolved(&world, target, &id).await;
        r
    }

    // ───────────────────────── actions ─────────────────────────

    /// Click the first match (left button).
    pub async fn click(&self, selector: &str, opts: ActionOptions) -> Result<(), BrowserError> {
        self.click_target(
            "click",
            &Target::Selector(Selector::parse(selector)),
            MouseButton::Left,
            1,
            &opts,
        )
        .await
    }

    /// Double-click the first match.
    pub async fn dblclick(&self, selector: &str, opts: ActionOptions) -> Result<(), BrowserError> {
        self.click_target(
            "dblclick",
            &Target::Selector(Selector::parse(selector)),
            MouseButton::Left,
            2,
            &opts,
        )
        .await
    }

    /// Right-click the first match.
    pub async fn right_click(
        &self,
        selector: &str,
        opts: ActionOptions,
    ) -> Result<(), BrowserError> {
        self.click_target(
            "right_click",
            &Target::Selector(Selector::parse(selector)),
            MouseButton::Right,
            1,
            &opts,
        )
        .await
    }

    /// Move the mouse over the first match.
    pub async fn hover(&self, selector: &str, opts: ActionOptions) -> Result<(), BrowserError> {
        self.hover_target(&Target::Selector(Selector::parse(selector)), &opts)
            .await
    }

    /// Focus the first match and type `text` key by key (characters the
    /// US layout cannot produce go through `Input.insertText`).
    pub async fn type_text(
        &self,
        selector: &str,
        text: &str,
        opts: ActionOptions,
    ) -> Result<(), BrowserError> {
        self.type_target(&Target::Selector(Selector::parse(selector)), text, &opts)
            .await
    }

    /// Replace the content of the first match with `text` (select all +
    /// `Input.insertText`; empty text deletes).
    pub async fn fill(
        &self,
        selector: &str,
        text: &str,
        opts: ActionOptions,
    ) -> Result<(), BrowserError> {
        self.fill_target(&Target::Selector(Selector::parse(selector)), text, &opts)
            .await
    }

    /// Press a key (`Enter`, `Ctrl+a`, …) on whatever has focus.
    pub async fn press(&self, key: &str) -> Result<(), BrowserError> {
        let press = input::parse_combo(key)?;
        self.mark_action();
        input::press_key(&self.session()?, &press).await
    }

    /// Focus the first match, then press a key.
    pub async fn press_on(
        &self,
        selector: &str,
        key: &str,
        opts: ActionOptions,
    ) -> Result<(), BrowserError> {
        self.press_target(&Target::Selector(Selector::parse(selector)), key, &opts)
            .await
    }

    /// Focus the first match.
    pub async fn focus(&self, selector: &str, opts: ActionOptions) -> Result<(), BrowserError> {
        let target = Target::Selector(Selector::parse(selector));
        let (world, id) = self.wait_attached("focus", &target, &opts).await?;
        let r = world.call_on(&id, FOCUS, vec![]).await.map(|_| ());
        world.release(&id).await;
        r
    }

    /// Check a checkbox / radio (clicks only if not already checked).
    pub async fn check(&self, selector: &str, opts: ActionOptions) -> Result<(), BrowserError> {
        self.set_checked_target(&Target::Selector(Selector::parse(selector)), true, &opts)
            .await
    }

    /// Uncheck a checkbox.
    pub async fn uncheck(&self, selector: &str, opts: ActionOptions) -> Result<(), BrowserError> {
        self.set_checked_target(&Target::Selector(Selector::parse(selector)), false, &opts)
            .await
    }

    /// Select option(s) of a `<select>` by value, label or text; returns
    /// the values actually selected.
    pub async fn select(
        &self,
        selector: &str,
        values: &[&str],
        opts: ActionOptions,
    ) -> Result<Vec<String>, BrowserError> {
        self.select_target(&Target::Selector(Selector::parse(selector)), values, &opts)
            .await
    }

    /// Scroll the first match into view.
    pub async fn scroll_into_view(
        &self,
        selector: &str,
        opts: ActionOptions,
    ) -> Result<(), BrowserError> {
        self.scroll_target(&Target::Selector(Selector::parse(selector)), &opts)
            .await
    }

    /// `window.scrollBy(x, y)`.
    pub async fn scroll_by(&self, x: f64, y: f64) -> Result<(), BrowserError> {
        self.eval_fn(
            "function(x, y) { window.scrollBy(x, y); }",
            vec![json!(x), json!(y)],
        )
        .await
        .map(|_| ())
    }

    /// `window.scrollTo(x, y)`.
    pub async fn scroll_to(&self, x: f64, y: f64) -> Result<(), BrowserError> {
        self.eval_fn(
            "function(x, y) { window.scrollTo(x, y); }",
            vec![json!(x), json!(y)],
        )
        .await
        .map(|_| ())
    }

    // ───────────────────────── readers ─────────────────────────

    /// `innerText` of the first match (waits for it to be attached).
    pub async fn text(&self, selector: &str, opts: ActionOptions) -> Result<String, BrowserError> {
        self.read_target(
            "text",
            &Target::Selector(Selector::parse(selector)),
            TEXT_OF,
            vec![],
            &opts,
        )
        .await
        .map(|v| v.as_str().unwrap_or("").to_owned())
    }

    /// `outerHTML` of the first match.
    pub async fn html(&self, selector: &str, opts: ActionOptions) -> Result<String, BrowserError> {
        self.read_target(
            "html",
            &Target::Selector(Selector::parse(selector)),
            "function() { return this.outerHTML; }",
            vec![],
            &opts,
        )
        .await
        .map(|v| v.as_str().unwrap_or("").to_owned())
    }

    /// Attribute value of the first match (`None` when absent).
    pub async fn attr(
        &self,
        selector: &str,
        name: &str,
        opts: ActionOptions,
    ) -> Result<Option<String>, BrowserError> {
        self.read_target(
            "attr",
            &Target::Selector(Selector::parse(selector)),
            "function(n) { return this.getAttribute(n); }",
            vec![json!(name)],
            &opts,
        )
        .await
        .map(|v| v.as_str().map(str::to_owned))
    }

    /// `value` of the first match (inputs, selects, textareas).
    pub async fn value(&self, selector: &str, opts: ActionOptions) -> Result<String, BrowserError> {
        self.read_target(
            "value",
            &Target::Selector(Selector::parse(selector)),
            "function() { return this.value == null ? '' : String(this.value); }",
            vec![],
            &opts,
        )
        .await
        .map(|v| v.as_str().unwrap_or("").to_owned())
    }

    /// Whether at least one element matches right now (no wait).
    pub async fn exists(&self, selector: &str) -> Result<bool, BrowserError> {
        Ok(self.count(selector).await? > 0)
    }

    /// Number of matches right now (no wait).
    pub async fn count(&self, selector: &str) -> Result<usize, BrowserError> {
        let sel = Selector::parse(selector);
        let (kind, text) = sel.parts();
        let v = self
            .with_world(|w| {
                let decl = format!(
                    "function(k, s) {{ const r = this[{:?}](k, s, true); return r.length; }}",
                    w.resolver
                );
                async move { w.call(&decl, vec![json!(kind), json!(text)]).await }
            })
            .await?;
        Ok(v.as_u64().unwrap_or(0) as usize)
    }

    /// Every match right now (no wait) as element handles.
    pub async fn all(&self, selector: &str) -> Result<Vec<Element>, BrowserError> {
        let sel = Selector::parse(selector);
        let (world, ids) = self
            .with_world(|w| {
                let sel = sel.clone();
                async move {
                    let ids = w.resolve(&sel, true).await?;
                    Ok((w, ids))
                }
            })
            .await?;
        Ok(ids
            .into_iter()
            .enumerate()
            .map(|(i, object_id)| Element {
                page: self.clone(),
                world: world.clone(),
                object_id,
                label: format!("{}[{i}]", sel.source()),
            })
            .collect())
    }

    /// The first match right now (no wait), if any.
    pub async fn first(&self, selector: &str) -> Result<Option<Element>, BrowserError> {
        Ok(self.all(selector).await?.into_iter().next())
    }

    // ───────────────────────── waits ─────────────────────────

    /// Wait until the first match is attached and visible; returns it.
    pub async fn wait(&self, selector: &str, opts: ActionOptions) -> Result<Element, BrowserError> {
        let sel = Selector::parse(selector);
        let target = Target::Selector(sel.clone());
        let started = Instant::now();
        let (deadline, _) = self.deadline(&opts);
        let mut last_state = String::from("not found");
        loop {
            let world = self.world().await?;
            let step = async {
                let Some(id) = self.locate(&world, &target).await? else {
                    return Ok::<_, BrowserError>(Err("not found".to_owned()));
                };
                let r = world.call_on(&id, READY_CHECK, vec![]).await?;
                let state = r["state"].as_str().unwrap_or("unknown").to_owned();
                if matches!(state.as_str(), "ready" | "moving" | "disabled") {
                    Ok(Ok(id))
                } else {
                    world.release(&id).await;
                    Ok(Err(state))
                }
            };
            match step.await {
                Ok(Ok(id)) => {
                    return Ok(Element {
                        page: self.clone(),
                        world,
                        object_id: id,
                        label: sel.source(),
                    })
                }
                Ok(Err(state)) => last_state = state,
                Err(e) if context_lost(&e) => self.invalidate_world(),
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Err(self.timeout_error("wait", &target, started, &last_state));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Wait until no match is attached and visible.
    pub async fn wait_gone(&self, selector: &str, opts: ActionOptions) -> Result<(), BrowserError> {
        let sel = Selector::parse(selector);
        let target = Target::Selector(sel.clone());
        let started = Instant::now();
        let (deadline, _) = self.deadline(&opts);
        loop {
            let world = self.world().await?;
            let step = async {
                let Some(id) = self.locate(&world, &target).await? else {
                    return Ok::<_, BrowserError>(true);
                };
                let r = world.call_on(&id, READY_CHECK, vec![]).await?;
                world.release(&id).await;
                let state = r["state"].as_str().unwrap_or("unknown");
                Ok(matches!(state, "detached" | "hidden"))
            };
            match step.await {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(e) if context_lost(&e) => self.invalidate_world(),
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Err(self.timeout_error("wait_gone", &target, started, "still visible"));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Wait until the first match's text contains `needle`
    /// (case-sensitive, whitespace-trimmed).
    pub async fn wait_text(
        &self,
        selector: &str,
        needle: &str,
        opts: ActionOptions,
    ) -> Result<(), BrowserError> {
        let sel = Selector::parse(selector);
        let target = Target::Selector(sel.clone());
        let started = Instant::now();
        let (deadline, _) = self.deadline(&opts);
        let mut last = String::from("not found");
        loop {
            let world = self.world().await?;
            let step = async {
                let Some(id) = self.locate(&world, &target).await? else {
                    return Ok::<_, BrowserError>(Err("not found".to_owned()));
                };
                let r = world.call_on(&id, TEXT_OF, vec![]).await?;
                world.release(&id).await;
                let text = r.as_str().unwrap_or("").trim().to_owned();
                if text.contains(needle.trim()) {
                    Ok(Ok(()))
                } else {
                    Ok(Err(format!("text is {:?}", truncate(&text, 60))))
                }
            };
            match step.await {
                Ok(Ok(())) => return Ok(()),
                Ok(Err(state)) => last = state,
                Err(e) if context_lost(&e) => self.invalidate_world(),
                Err(e) => return Err(e),
            }
            if Instant::now() >= deadline {
                return Err(self.timeout_error("wait_text", &target, started, &last));
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Wait until the URL matches `pattern`: a glob (`*` wildcard; a bare
    /// string must match the whole URL) or a regex with the `re:` prefix.
    pub async fn wait_url(&self, pattern: &str, opts: ActionOptions) -> Result<(), BrowserError> {
        let matcher = UrlPattern::parse(pattern)?;
        let started = Instant::now();
        let (deadline, _) = self.deadline(&opts);
        loop {
            // A navigation in flight makes `location.href` fail briefly;
            // treat that as "unknown" and keep polling.
            let url = match self.url().await {
                Ok(u) => u,
                Err(e) if context_lost(&e) => String::new(),
                Err(BrowserError::Cdp(surf_cdp::CdpError::Protocol { .. })) => String::new(),
                Err(e) => return Err(e),
            };
            if matcher.matches(&url) {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(BrowserError::Timeout {
                    action: "wait_url".into(),
                    selector: Some(pattern.to_owned()),
                    waited_ms: started.elapsed().as_millis() as u64,
                    last_state: Some(format!("url is {url:?}")),
                });
            }
            tokio::time::sleep(POLL_INTERVAL * 2).await;
        }
    }

    /// Plain sleep (`wait(2s)`).
    pub async fn wait_for(&self, d: Duration) {
        tokio::time::sleep(d).await;
    }
}

impl Element {
    /// Wrap a handle the caller already holds (an `objectId` in `world`, on
    /// `page`) — used by the runtime's `on element_appears` observer, which
    /// keeps matched elements in an isolated-world array and hands their
    /// handles to handler bodies. `label` is shown in errors.
    pub fn from_handle(page: Page, world: World, object_id: String, label: String) -> Element {
        Element {
            page,
            world,
            object_id,
            label,
        }
    }

    /// `selector[index]`.
    pub fn label(&self) -> &str {
        &self.label
    }

    fn target(&self) -> Target {
        Target::Handle {
            object_id: self.object_id.clone(),
            label: self.label.clone(),
        }
    }

    /// Click (auto-waits for visible / stable / enabled).
    pub async fn click(&self, opts: ActionOptions) -> Result<(), BrowserError> {
        self.page
            .click_target("click", &self.target(), MouseButton::Left, 1, &opts)
            .await
    }

    /// Double-click.
    pub async fn dblclick(&self, opts: ActionOptions) -> Result<(), BrowserError> {
        self.page
            .click_target("dblclick", &self.target(), MouseButton::Left, 2, &opts)
            .await
    }

    /// Hover.
    pub async fn hover(&self, opts: ActionOptions) -> Result<(), BrowserError> {
        self.page.hover_target(&self.target(), &opts).await
    }

    /// Type key by key.
    pub async fn type_text(&self, text: &str, opts: ActionOptions) -> Result<(), BrowserError> {
        self.page.type_target(&self.target(), text, &opts).await
    }

    /// Replace content.
    pub async fn fill(&self, text: &str, opts: ActionOptions) -> Result<(), BrowserError> {
        self.page.fill_target(&self.target(), text, &opts).await
    }

    /// Focus then press a key.
    pub async fn press(&self, key: &str, opts: ActionOptions) -> Result<(), BrowserError> {
        self.page.press_target(&self.target(), key, &opts).await
    }

    /// Check.
    pub async fn check(&self, opts: ActionOptions) -> Result<(), BrowserError> {
        self.page
            .set_checked_target(&self.target(), true, &opts)
            .await
    }

    /// Uncheck.
    pub async fn uncheck(&self, opts: ActionOptions) -> Result<(), BrowserError> {
        self.page
            .set_checked_target(&self.target(), false, &opts)
            .await
    }

    /// Select options.
    pub async fn select(
        &self,
        values: &[&str],
        opts: ActionOptions,
    ) -> Result<Vec<String>, BrowserError> {
        self.page.select_target(&self.target(), values, &opts).await
    }

    /// Scroll into view.
    pub async fn scroll_into_view(&self) -> Result<(), BrowserError> {
        self.page
            .scroll_target(&self.target(), &ActionOptions::default())
            .await
    }

    /// Focus.
    pub async fn focus(&self) -> Result<(), BrowserError> {
        self.world
            .call_on(&self.object_id, FOCUS, vec![])
            .await
            .map(|_| ())
    }

    /// `innerText`.
    pub async fn text(&self) -> Result<String, BrowserError> {
        self.world
            .call_on(&self.object_id, TEXT_OF, vec![])
            .await
            .map(|v| v.as_str().unwrap_or("").to_owned())
    }

    /// `outerHTML`.
    pub async fn html(&self) -> Result<String, BrowserError> {
        self.world
            .call_on(
                &self.object_id,
                "function() { return this.outerHTML; }",
                vec![],
            )
            .await
            .map(|v| v.as_str().unwrap_or("").to_owned())
    }

    /// Attribute value.
    pub async fn attr(&self, name: &str) -> Result<Option<String>, BrowserError> {
        self.world
            .call_on(
                &self.object_id,
                "function(n) { return this.getAttribute(n); }",
                vec![json!(name)],
            )
            .await
            .map(|v| v.as_str().map(str::to_owned))
    }

    /// `value`.
    pub async fn value(&self) -> Result<String, BrowserError> {
        self.world
            .call_on(
                &self.object_id,
                "function() { return this.value == null ? '' : String(this.value); }",
                vec![],
            )
            .await
            .map(|v| v.as_str().unwrap_or("").to_owned())
    }

    /// Whether the element is still attached and visible.
    pub async fn is_visible(&self) -> Result<bool, BrowserError> {
        let r = self
            .world
            .call_on(&self.object_id, READY_CHECK, vec![])
            .await?;
        Ok(matches!(
            r["state"].as_str(),
            Some("ready" | "moving" | "disabled")
        ))
    }

    /// Call a function with `this` = the element; JSON result.
    pub async fn eval_fn(
        &self,
        function_source: &str,
        args: Vec<Value>,
    ) -> Result<Value, BrowserError> {
        self.world
            .call_on(&self.object_id, function_source, args)
            .await
    }

    /// Descendants matching `selector` (CSS / text / xpath, scoped to this
    /// element for CSS).
    pub async fn all(&self, selector: &str) -> Result<Vec<Element>, BrowserError> {
        let sel = Selector::parse(selector);
        let (kind, text) = sel.parts();
        let decl = match kind {
            "css" => "function(s) { return Array.from(this.querySelectorAll(s)); }".to_owned(),
            _ => format!(
                "function(s, k) {{ const all = globalThis[{:?}](k, s, true); return all.filter((e) => this.contains(e) && e !== this); }}",
                self.world.resolver
            ),
        };
        let list = self
            .world
            .call_on_handle(
                &self.object_id,
                &decl,
                by_value(vec![json!(text), json!(kind)]),
            )
            .await?;
        let Some(list_id) = list.object_id else {
            return Ok(Vec::new());
        };
        let props = self
            .world
            .session
            .send(surf_cdp::protocol::runtime::GetProperties {
                object_id: list_id.clone(),
                own_properties: Some(true),
                ..Default::default()
            })
            .await?;
        self.world.release(&list_id).await;
        let mut indexed: Vec<(usize, String)> = props
            .result
            .into_iter()
            .filter_map(|p| Some((p.name.parse().ok()?, p.value?.object_id?)))
            .collect();
        indexed.sort_by_key(|(i, _)| *i);
        Ok(indexed
            .into_iter()
            .map(|(i, object_id)| Element {
                page: self.page.clone(),
                world: self.world.clone(),
                object_id,
                label: format!("{} {}[{i}]", self.label, sel.source()),
            })
            .collect())
    }
}

/// Type `text` into the focused element: US-layout characters as key
/// events (`keyDown` with text + `keyUp`), anything else via
/// `Input.insertText`; optional jittered delay between keys.
async fn type_keys(world: &World, text: &str, delay: Option<Duration>) -> Result<(), BrowserError> {
    let session = &world.session;
    let mut jitter = 0u64;
    for (i, c) in text.chars().enumerate() {
        if i > 0 {
            if let Some(d) = delay {
                // ±50 % jitter from a cheap LCG; no crate needed.
                jitter = jitter
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407 + i as u64);
                let f = 0.5 + (jitter >> 40) as f64 / (1u64 << 24) as f64;
                tokio::time::sleep(d.mul_f64(f)).await;
            }
        }
        match input::char_key(c) {
            Some(key) => {
                input::press_key(session, &KeyPress { key, modifiers: 0 }).await?;
            }
            None => input::insert_text(session, &c.to_string()).await?,
        }
    }
    Ok(())
}

/// Centre of a content quad (`[x1,y1,x2,y2,x3,y3,x4,y4]`), or `None` when
/// it has no area.
fn quad_centre(q: &[f64]) -> Option<(f64, f64)> {
    if q.len() < 8 {
        return None;
    }
    let xs = [q[0], q[2], q[4], q[6]];
    let ys = [q[1], q[3], q[5], q[7]];
    let (minx, maxx) = (
        xs.iter().cloned().fold(f64::MAX, f64::min),
        xs.iter().cloned().fold(f64::MIN, f64::max),
    );
    let (miny, maxy) = (
        ys.iter().cloned().fold(f64::MAX, f64::min),
        ys.iter().cloned().fold(f64::MIN, f64::max),
    );
    if maxx - minx < 1.0 || maxy - miny < 1.0 {
        return None;
    }
    Some(((minx + maxx) / 2.0, (miny + maxy) / 2.0))
}

fn short(e: &surf_cdp::CdpError) -> String {
    match e {
        surf_cdp::CdpError::Protocol { message, .. } => message.clone(),
        other => other.to_string(),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_owned()
    } else {
        let cut: String = s.chars().take(n).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_centres() {
        assert_eq!(
            quad_centre(&[10.0, 10.0, 30.0, 10.0, 30.0, 20.0, 10.0, 20.0]),
            Some((20.0, 15.0))
        );
        assert_eq!(quad_centre(&[1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]), None);
        assert_eq!(quad_centre(&[1.0]), None);
    }

    #[test]
    fn url_matchers() {
        assert!(UrlPattern::parse("*/done")
            .unwrap()
            .matches("http://x/a/done"));
        assert!(!UrlPattern::parse("*/done")
            .unwrap()
            .matches("http://x/a/done?x"));
        assert!(UrlPattern::parse("re:done\\?")
            .unwrap()
            .matches("http://x/done?x"));
        assert!(UrlPattern::parse("re:(").is_err());
    }
}
