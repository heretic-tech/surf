//! Frames: the main frame and its `<iframe>` descendants, each with its
//! own isolated world.
//!
//! A [`Frame`] carries the whole action / reader / wait surface of
//! `actions.rs` (that module is `impl Frame`; [`Page`] forwards to its
//! main frame). Worlds are per frame: `Page.createIsolatedWorld{frameId}`
//! already takes any frame id, so a child frame's world is one more entry
//! in the page's cache, keyed by frame id and tagged with the frame's
//! navigation epoch (bumped by the page's event task on every
//! `Page.frameNavigated` for that frame, so a world is never reused across
//! the frame's navigations). Nothing new is enabled: `Page` is already on
//! and `frameAttached` / `frameDetached` / `frameNavigated` ride on it.
//!
//! Limits: frames that Chrome renders in another process (cross-site
//! iframes with site isolation) are separate targets and are not
//! reachable through the page's session; `frame()` lists them but their
//! world creation fails with Chrome's `No frame for given id found`.
//! Navigation helpers (`goto`, `reload`, …) stay on the page.

use crate::actions::POLL_INTERVAL;
use crate::error::BrowserError;
use crate::network::UrlPattern;
use crate::page::Page;
use crate::world::{context_lost, World};
use serde_json::Value;
use std::future::Future;
use std::time::Instant;
use surf_cdp::protocol::page;

/// Identity of one frame as `Page.getFrameTree` reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameInfo {
    /// `Page.FrameId`.
    pub id: String,
    /// Parent frame id (`None` for the main frame).
    pub parent_id: Option<String>,
    /// The `name` attribute of the owning `<iframe>` (empty when unnamed).
    pub name: String,
    /// Document URL at the time of the snapshot.
    pub url: String,
}

/// How [`Page::frame`] picks a child frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameRef {
    /// `<iframe name="…">`.
    Name(String),
    /// A URL glob (`*` wildcard, anchored) or `re:` pattern on the frame's
    /// document URL.
    UrlGlob(String),
    /// Position among the child frames in tree order (depth-first, the
    /// main frame excluded), 0-based.
    Index(usize),
}

impl std::fmt::Display for FrameRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameRef::Name(n) => write!(f, "name={n}"),
            FrameRef::UrlGlob(u) => write!(f, "url={u}"),
            FrameRef::Index(i) => write!(f, "#{i}"),
        }
    }
}

/// One frame of a page. Cheap to clone; every clone shares the page's
/// world cache. The main frame is [`Page::main_frame`].
#[derive(Clone)]
pub struct Frame {
    page: Page,
    info: FrameInfo,
    main: bool,
}

impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Frame")
            .field("page", &self.page.index())
            .field("id", &self.info.id)
            .field("name", &self.info.name)
            .field("url", &self.info.url)
            .field("main", &self.main)
            .finish()
    }
}

impl Frame {
    /// The page this frame belongs to.
    pub fn page(&self) -> &Page {
        &self.page
    }

    /// `Page.FrameId`.
    pub fn frame_id(&self) -> &str {
        &self.info.id
    }

    /// Whether this is the page's main frame.
    pub fn is_main(&self) -> bool {
        self.main
    }

    /// The `<iframe name>` (empty when unnamed; empty for the main frame).
    pub fn name(&self) -> &str {
        &self.info.name
    }

    /// Snapshot of the frame as listed (name, URL, parent).
    pub fn info(&self) -> &FrameInfo {
        &self.info
    }

    /// How the frame is shown in errors: `main`, `name`, or the id.
    pub fn label(&self) -> String {
        if self.main {
            "main".into()
        } else if !self.info.name.is_empty() {
            self.info.name.clone()
        } else {
            self.info.id.clone()
        }
    }

    fn epoch(&self) -> u64 {
        self.page
            .0
            .shared
            .frame_epochs
            .lock()
            .expect("frame epochs")
            .get(&self.info.id)
            .copied()
            .unwrap_or(0)
    }

    /// The frame's isolated world (created lazily; re-created after the
    /// frame navigates). The main frame's is [`Page::world`].
    pub async fn world(&self) -> Result<World, BrowserError> {
        if self.main {
            return self.page.world().await;
        }
        let epoch = self.epoch();
        let cached = self
            .page
            .0
            .frame_worlds
            .borrow()
            .get(&self.info.id)
            .cloned();
        if let Some((e, w)) = cached {
            if e == epoch {
                self.page.flush_garbage(&w.session);
                return Ok(w);
            }
        }
        let w = World::create(self.page.session()?, &self.info.id).await?;
        self.page
            .0
            .frame_worlds
            .borrow_mut()
            .insert(self.info.id.clone(), (epoch, w.clone()));
        Ok(w)
    }

    /// Forget the cached world (next use creates a new one).
    pub fn invalidate_world(&self) {
        if self.main {
            self.page.invalidate_world();
        } else {
            self.page.0.frame_worlds.borrow_mut().remove(&self.info.id);
        }
    }

    /// Run `f` against the world; if Chrome reports the execution context
    /// gone (navigation in between), create a fresh world and retry once.
    pub async fn with_world<T, F, Fut>(&self, f: F) -> Result<T, BrowserError>
    where
        F: Fn(World) -> Fut,
        Fut: Future<Output = Result<T, BrowserError>>,
    {
        let w = self.world().await?;
        match f(w).await {
            Err(e) if context_lost(&e) => {
                tracing::debug!("isolated world gone ({e}); re-creating");
                self.invalidate_world();
                let w = self.world().await?;
                f(w).await
            }
            r => r,
        }
    }

    /// Evaluate a JavaScript expression in the frame's isolated world.
    pub async fn eval(&self, expression: &str) -> Result<Value, BrowserError> {
        self.with_world(|w| async move { w.eval(expression).await })
            .await
    }

    /// Call a JavaScript function source with JSON arguments in the
    /// frame's isolated world.
    pub async fn eval_fn(
        &self,
        function_source: &str,
        args: Vec<Value>,
    ) -> Result<Value, BrowserError> {
        self.with_world(|w| {
            let args = args.clone();
            async move { w.call(function_source, args).await }
        })
        .await
    }

    /// `location.href` of the frame's document.
    pub async fn url(&self) -> Result<String, BrowserError> {
        Ok(self
            .eval("location.href")
            .await?
            .as_str()
            .unwrap_or("")
            .to_owned())
    }
}

/// Flatten a `Page.getFrameTree` result: the root first, then children
/// depth-first in tree order.
fn flatten(tree: &page::FrameTree, parent: Option<&str>, out: &mut Vec<FrameInfo>) {
    out.push(FrameInfo {
        id: tree.frame.id.clone(),
        parent_id: parent.map(str::to_owned),
        name: tree.frame.name.clone().unwrap_or_default(),
        url: tree.frame.url.clone(),
    });
    for child in tree.child_frames.iter().flatten() {
        flatten(child, Some(&tree.frame.id), out);
    }
}

impl Page {
    /// The main frame as a [`Frame`] (no round trip).
    pub fn main_frame(&self) -> Frame {
        let id = self.frame_id().unwrap_or_default();
        Frame {
            page: self.clone(),
            info: FrameInfo {
                id,
                parent_id: None,
                name: String::new(),
                url: self.last_navigated_url(),
            },
            main: true,
        }
    }

    /// Every frame right now (`Page.getFrameTree`): the main frame first,
    /// then child frames depth-first in tree order.
    pub async fn frames(&self) -> Result<Vec<Frame>, BrowserError> {
        let tree = self.session()?.send(page::GetFrameTree {}).await?;
        let main_id = self.frame_id()?;
        let mut infos = Vec::new();
        flatten(&tree.frame_tree, None, &mut infos);
        Ok(infos
            .into_iter()
            .map(|info| Frame {
                page: self.clone(),
                main: info.id == main_id,
                info,
            })
            .collect())
    }

    /// The child frame matching `which`, waiting up to the page timeout
    /// for it to appear (iframes attach asynchronously). The main frame is
    /// never returned here — use [`main_frame`](Self::main_frame).
    pub async fn frame(&self, which: FrameRef) -> Result<Frame, BrowserError> {
        let started = Instant::now();
        let deadline = started + self.timeout();
        let matcher = match &which {
            FrameRef::UrlGlob(p) => Some(UrlPattern::parse(p)?),
            _ => None,
        };
        loop {
            let children: Vec<Frame> = self
                .frames()
                .await?
                .into_iter()
                .filter(|f| !f.main)
                .collect();
            let hit = match &which {
                FrameRef::Name(n) => children.iter().find(|f| f.info.name == *n),
                FrameRef::UrlGlob(_) => children
                    .iter()
                    .find(|f| matcher.as_ref().is_some_and(|m| m.matches(&f.info.url))),
                FrameRef::Index(i) => children.get(*i),
            };
            if let Some(f) = hit {
                return Ok(f.clone());
            }
            if Instant::now() >= deadline {
                let listed: Vec<String> = children
                    .iter()
                    .map(|f| {
                        if f.info.name.is_empty() {
                            f.info.url.clone()
                        } else {
                            format!("{} ({})", f.info.name, f.info.url)
                        }
                    })
                    .collect();
                return Err(BrowserError::Timeout {
                    action: "frame".into(),
                    selector: Some(which.to_string()),
                    waited_ms: started.elapsed().as_millis() as u64,
                    last_state: Some(if listed.is_empty() {
                        "no child frames".into()
                    } else {
                        format!("frames: {}", listed.join(", "))
                    }),
                });
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}
