//! The [`Browser`] handle: a logical browser that owns a launched process
//! (or an attached connection), a registry of [`Page`]s in creation order
//! with optional names, and browser contexts for per-page proxies /
//! isolation.
//!
//! Page rules (Decision 6): `page(n)` / `page("name")` auto-create;
//! `page(n)` creates every missing page up to `n`; names map to the index
//! of the page created for them. [`Browser::sole_page`] serves bare
//! actions: it creates page 1 when none exists and fails with
//! [`BrowserError::Ambiguous`] (listing `1, 2, "login"`) when several are
//! open.
//!
//! Every page is `Target.createTarget{url: "about:blank"}` →
//! `Target.attachToTarget{flatten: true}` → [`Page::attach`]. A page with
//! its own proxy (or `isolated: true`) first gets
//! `Target.createBrowserContext{proxyServer, proxyBypassList}`; the
//! context is disposed when the page closes. Proxy credentials (launch
//! level or per page) are answered through [`crate::network::ProxyAuth`].

use crate::error::BrowserError;
use crate::launch::{launch, CdpMode, Credentials, LaunchOptions, Launched, ProxySpec};
use crate::page::{Backing, Migration, Page};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use surf_cdp::protocol::target;
use surf_cdp::transport::ws::WsTransport;
use surf_cdp::{Connection, Session};

/// Options for [`Browser::new_page`].
#[derive(Debug, Clone, Default)]
pub struct NewPageOptions {
    /// Proxy for this page only (`scheme://[user:pass@]host:port`): the
    /// page gets its own browser context with `proxyServer`.
    pub proxy: Option<String>,
    /// Give the page its own browser context (separate cookies / storage)
    /// even without a proxy.
    pub isolated: bool,
    /// Register under this name (`page("login")`).
    pub name: Option<String>,
}

/// Where [`Browser::rebind_page_to`] puts the page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RebindTarget {
    /// A new target in the page's current browser context (cookies and
    /// storage stay shared with the old target).
    #[default]
    SameContext,
    /// A new, empty browser context — nothing shared — keeping the proxy
    /// the page currently has (if any). `fresh: true` retries.
    FreshContext,
    /// A new, empty browser context behind `proxy`
    /// (`scheme://[user:pass@]host:port`). `shift_proxy()`.
    Proxy(String),
}

/// How the browser was obtained.
enum Origin {
    Launched(Box<Launched>),
    Attached { url: String },
}

/// A logical browser: owns one process (or connection), hands out
/// [`Page`]s, and handles graceful shutdown.
pub struct Browser {
    opts: LaunchOptions,
    origin: RefCell<Option<Origin>>,
    conn: Arc<Connection>,
    pages: RefCell<Vec<Page>>,
    /// name → 1-based page index.
    names: RefCell<HashMap<String, usize>>,
    /// Launch-level proxy credentials (default browser context).
    credentials: Option<Credentials>,
    /// Browser contexts created for pages: target id → context id.
    contexts: RefCell<HashMap<String, String>>,
    /// Proxy URL per private context: context id → proxy.
    context_proxies: RefCell<HashMap<String, String>>,
    next_index: Cell<usize>,
    closed: Cell<bool>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("pages", &self.pages.borrow().len())
            .field("closed", &self.closed.get())
            .finish()
    }
}

impl Browser {
    /// Launch (or attach, per `opts.cdp`) and return the handle. Nothing
    /// else happens: pages are created on first use.
    pub async fn launch(opts: LaunchOptions) -> Result<Rc<Browser>, BrowserError> {
        if let CdpMode::Attach(url) = &opts.cdp {
            let url = url.clone();
            return Browser::connect(&url, opts).await;
        }
        let cfg = opts.resolve()?;
        let launched = launch(cfg).await?;
        let conn = launched.connection.clone();
        let credentials = launched.proxy_credentials.clone();
        Ok(Rc::new(Browser {
            opts,
            origin: RefCell::new(Some(Origin::Launched(Box::new(launched)))),
            conn,
            pages: RefCell::new(Vec::new()),
            names: RefCell::new(HashMap::new()),
            credentials,
            contexts: RefCell::new(HashMap::new()),
            context_proxies: RefCell::new(HashMap::new()),
            next_index: Cell::new(1),
            closed: Cell::new(false),
        }))
    }

    /// Attach to a running browser or remote provider over websocket
    /// (`cdp: "ws://…"`, `pool: "wss://…"`). Never closed by Surf; pages
    /// Surf created are closed on [`close`](Self::close).
    pub async fn connect(ws_url: &str, opts: LaunchOptions) -> Result<Rc<Browser>, BrowserError> {
        let transport = WsTransport::connect(ws_url)
            .await
            .map_err(|e| BrowserError::Launch(format!("could not connect to {ws_url}: {e}")))?;
        let conn = Connection::new(Box::new(transport));
        let root = conn.root().with_timeout(Some(Duration::from_secs(10)));
        let v = root
            .send(surf_cdp::protocol::browser::GetVersion {})
            .await?;
        tracing::info!("attached to {} at {ws_url}", v.product);
        let credentials = opts
            .proxy
            .as_deref()
            .map(ProxySpec::parse)
            .transpose()?
            .and_then(|p| p.credentials);
        Ok(Rc::new(Browser {
            opts,
            origin: RefCell::new(Some(Origin::Attached {
                url: ws_url.to_owned(),
            })),
            conn,
            pages: RefCell::new(Vec::new()),
            names: RefCell::new(HashMap::new()),
            credentials,
            contexts: RefCell::new(HashMap::new()),
            context_proxies: RefCell::new(HashMap::new()),
            next_index: Cell::new(1),
            closed: Cell::new(false),
        }))
    }

    /// Options this browser was launched with.
    pub fn options(&self) -> &LaunchOptions {
        &self.opts
    }

    /// The CDP connection.
    pub fn connection(&self) -> &Arc<Connection> {
        &self.conn
    }

    /// The browser-level session.
    pub fn root(&self) -> Session {
        self.conn
            .root()
            .with_timeout(Some(self.opts.timeout + Duration::from_secs(5)))
    }

    /// Whether this browser was launched by Surf (as opposed to attached).
    pub fn is_launched(&self) -> bool {
        matches!(&*self.origin.borrow(), Some(Origin::Launched(_)))
    }

    /// The launched process, if any (pid, stderr tail, …).
    pub fn with_launched<T>(&self, f: impl FnOnce(&Launched) -> T) -> Option<T> {
        match &*self.origin.borrow() {
            Some(Origin::Launched(l)) => Some(f(l)),
            _ => None,
        }
    }

    // ───────────────────────── pages ─────────────────────────

    /// Create a new page (tab); see [`NewPageOptions`].
    pub async fn new_page(&self, opts: NewPageOptions) -> Result<Page, BrowserError> {
        if self.closed.get() {
            return Err(BrowserError::Cdp(surf_cdp::CdpError::Closed));
        }
        let root = self.root();
        let (context_id, credentials) = match (&opts.proxy, opts.isolated) {
            (Some(proxy), _) => {
                let spec = ProxySpec::parse(proxy)?;
                let ctx = root
                    .send(target::CreateBrowserContext {
                        proxy_server: Some(spec.server_arg()),
                        dispose_on_detach: Some(true),
                        ..Default::default()
                    })
                    .await?;
                (Some(ctx.browser_context_id), spec.credentials)
            }
            (None, true) => {
                let ctx = root
                    .send(target::CreateBrowserContext {
                        dispose_on_detach: Some(true),
                        ..Default::default()
                    })
                    .await?;
                (Some(ctx.browser_context_id), None)
            }
            (None, false) => (None, self.credentials.clone()),
        };
        let backing = match self.create_backing(context_id.clone()).await {
            Ok(b) => b,
            Err(e) => {
                if let Some(ctx) = &context_id {
                    let _ = root
                        .send(target::DisposeBrowserContext {
                            browser_context_id: ctx.clone(),
                        })
                        .await;
                }
                return Err(e);
            }
        };
        if let Some(ctx) = &context_id {
            self.contexts
                .borrow_mut()
                .insert(backing.target_id.clone(), ctx.clone());
            if let Some(proxy) = &opts.proxy {
                self.context_proxies
                    .borrow_mut()
                    .insert(ctx.clone(), proxy.clone());
            }
        }
        let index = self.next_index.get();
        self.next_index.set(index + 1);
        let page = Page::attach(index, backing, self.opts.timeout, credentials).await?;
        if let Some(name) = &opts.name {
            page.set_name(Some(name.clone()));
            self.names.borrow_mut().insert(name.clone(), index);
        }
        self.pages.borrow_mut().push(page.clone());
        tracing::debug!(
            "page {} created{}",
            page.label(),
            context_id
                .map(|c| format!(" in context {c}"))
                .unwrap_or_default()
        );
        Ok(page)
    }

    /// `Target.createTarget{url: "about:blank"}` in `context_id` + attach.
    pub async fn create_backing(
        &self,
        context_id: Option<String>,
    ) -> Result<Backing, BrowserError> {
        let root = self.root();
        let created = root
            .send(target::CreateTarget {
                url: "about:blank".into(),
                browser_context_id: context_id.clone(),
                ..Default::default()
            })
            .await?;
        let session = self.conn.attach(&created.target_id).await?;
        Backing::discover(session, created.target_id, context_id).await
    }

    /// `page(n)`: the page with creation index `n` (1-based); creates every
    /// missing page up to `n`. Indices never shift: a closed page's index
    /// stays taken and `page(n)` for it is [`BrowserError::PageClosed`].
    pub async fn page(&self, index: usize) -> Result<Page, BrowserError> {
        if index == 0 {
            return Err(BrowserError::Config {
                what: "page index".into(),
                reason: "pages are numbered from 1".into(),
            });
        }
        loop {
            let found = self
                .pages
                .borrow()
                .iter()
                .find(|p| p.index() == index)
                .cloned();
            if let Some(p) = found {
                return Ok(p);
            }
            if self.next_index.get() > index {
                return Err(BrowserError::PageClosed { index });
            }
            self.new_page(NewPageOptions::default()).await?;
        }
    }

    /// `page("login")`: the page registered under `name`; created (and
    /// named) when missing.
    pub async fn page_named(&self, name: &str) -> Result<Page, BrowserError> {
        let existing = self.names.borrow().get(name).copied();
        if let Some(i) = existing {
            if let Some(p) = self.pages.borrow().get(i - 1).cloned() {
                return Ok(p);
            }
        }
        self.new_page(NewPageOptions {
            name: Some(name.to_owned()),
            ..Default::default()
        })
        .await
    }

    /// The page bare actions resolve to: the only open page (created when
    /// none exists). With several open, [`BrowserError::Ambiguous`] lists
    /// them as `1, 2, "login"`.
    pub async fn sole_page(&self) -> Result<Page, BrowserError> {
        let pages: Vec<Page> = self.pages.borrow().clone();
        match pages.len() {
            0 => self.new_page(NewPageOptions::default()).await,
            1 => Ok(pages[0].clone()),
            _ => Err(BrowserError::Ambiguous {
                names: pages.iter().map(Page::label).collect(),
            }),
        }
    }

    /// All open pages in creation order.
    pub fn pages(&self) -> Vec<Page> {
        self.pages.borrow().clone()
    }

    /// Close a page and dispose of its private browser context, if any.
    pub async fn close_page(&self, page: &Page) -> Result<(), BrowserError> {
        let target_id = page.backing().map(|b| b.target_id);
        page.close().await?;
        self.pages
            .borrow_mut()
            .retain(|p| p.index() != page.index());
        if let Some(name) = page.name() {
            self.names.borrow_mut().remove(&name);
        }
        if let Some(ctx) = target_id.and_then(|t| self.contexts.borrow_mut().remove(&t)) {
            self.context_proxies.borrow_mut().remove(&ctx);
            if !self.conn.is_closed() {
                let _ = self
                    .root()
                    .send(target::DisposeBrowserContext {
                        browser_context_id: ctx,
                    })
                    .await;
            }
        }
        Ok(())
    }

    /// The proxy `page`'s private browser context was created with, if any
    /// (`new_page(proxy:)` / [`RebindTarget::Proxy`]). The launch-level
    /// `proxy:` is not reported here.
    pub fn page_proxy(&self, page: &Page) -> Option<String> {
        let ctx = page.backing()?.browser_context_id?;
        self.context_proxies.borrow().get(&ctx).cloned()
    }

    /// Move `page` onto a fresh target — in a new browser context with
    /// `proxy` when given, otherwise in the same context — migrating
    /// state per `migration` ([`Page::rebind`]). The old context (if
    /// private) is disposed afterwards.
    pub async fn rebind_page(
        &self,
        page: &Page,
        proxy: Option<&str>,
        migration: Migration,
    ) -> Result<(), BrowserError> {
        let target = match proxy {
            Some(p) => RebindTarget::Proxy(p.to_owned()),
            None => RebindTarget::SameContext,
        };
        self.rebind_page_to(page, target, migration).await
    }

    /// Move `page` onto a fresh target per `target` ([`RebindTarget`]),
    /// migrating state per `migration` ([`Page::rebind`]). A new context
    /// gets the proxy's credentials (or none); the old private context is
    /// disposed afterwards. The handle, index and name stay the same.
    pub async fn rebind_page_to(
        &self,
        page: &Page,
        target: RebindTarget,
        migration: Migration,
    ) -> Result<(), BrowserError> {
        let old = page.backing().ok_or(BrowserError::PageClosed {
            index: page.index(),
        })?;
        let root = self.root();
        let proxy = match &target {
            RebindTarget::SameContext => None,
            RebindTarget::FreshContext => self.page_proxy(page),
            RebindTarget::Proxy(p) => Some(p.clone()),
        };
        let spec = proxy.as_deref().map(ProxySpec::parse).transpose()?;
        let new_context = !matches!(target, RebindTarget::SameContext);
        let context_id = if new_context {
            Some(
                root.send(target::CreateBrowserContext {
                    proxy_server: spec.as_ref().map(ProxySpec::server_arg),
                    dispose_on_detach: Some(true),
                    ..Default::default()
                })
                .await?
                .browser_context_id,
            )
        } else {
            old.browser_context_id.clone()
        };
        let backing = match self.create_backing(context_id.clone()).await {
            Ok(b) => b,
            Err(e) => {
                if let (Some(ctx), true) = (&context_id, context_id != old.browser_context_id) {
                    let _ = root
                        .send(target::DisposeBrowserContext {
                            browser_context_id: ctx.clone(),
                        })
                        .await;
                }
                return Err(e);
            }
        };
        let new_target = backing.target_id.clone();
        if new_context {
            page.set_credentials(spec.as_ref().and_then(|s| s.credentials.clone()));
        }
        page.rebind(backing, migration).await?;
        let old_ctx = self.contexts.borrow_mut().remove(&old.target_id);
        if let Some(ctx) = &context_id {
            self.contexts.borrow_mut().insert(new_target, ctx.clone());
            if let (true, Some(p)) = (new_context, &proxy) {
                self.context_proxies
                    .borrow_mut()
                    .insert(ctx.clone(), p.clone());
            }
        }
        if let Some(ctx) = old_ctx {
            if Some(&ctx) != context_id.as_ref() {
                self.context_proxies.borrow_mut().remove(&ctx);
                let _ = root
                    .send(target::DisposeBrowserContext {
                        browser_context_id: ctx,
                    })
                    .await;
            }
        }
        Ok(())
    }

    /// Graceful shutdown. Launched: `Browser.close` → wait → `SIGTERM` →
    /// `SIGKILL`, temp profile removed (`profile:` kept). Attached: close
    /// the pages Surf created and drop the connection; the remote browser
    /// keeps running. Idempotent.
    pub async fn close(&self) -> Result<(), BrowserError> {
        if self.closed.replace(true) {
            return Ok(());
        }
        let pages: Vec<Page> = std::mem::take(&mut *self.pages.borrow_mut());
        let origin = self.origin.borrow_mut().take();
        match origin {
            Some(Origin::Launched(l)) => {
                // The process is going away: no per-page CDP traffic.
                l.close().await;
                for p in &pages {
                    p.forget();
                }
            }
            Some(Origin::Attached { url }) => {
                for p in &pages {
                    if let Err(e) = p.close().await {
                        tracing::debug!("closing page {} on {url}: {e}", p.index());
                    }
                }
                let contexts: Vec<String> =
                    self.contexts.borrow_mut().drain().map(|(_, c)| c).collect();
                for ctx in contexts {
                    let _ = self
                        .root()
                        .send(target::DisposeBrowserContext {
                            browser_context_id: ctx,
                        })
                        .await;
                }
                self.conn.close();
            }
            None => {}
        }
        self.names.borrow_mut().clear();
        Ok(())
    }
}
