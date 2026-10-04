# Architecture

Surf is a scripting language plus a Rust runtime that drives Chromium over
raw CDP without producing page-observable side effects. This document is the
map; `docs/language.md` is the language spec; `DECISIONS.md` records why.

## Crate map

```
crates/
  surf-syntax    lexer (INDENT/DEDENT) → parser (recursive descent + Pratt) → AST, ariadne diagnostics   [wasm32-clean]
  surf-vm        AST → bytecode; single-threaded async stack VM; Value; Host trait; stdlib             [wasm32-clean]
  surf-cdp       Transport (pipe | websocket), \0 framing, Connection (id/session mux), Session, Event, typed Command codegen   [Send + Sync]
  surf-browser   Chrome discovery, quiet launch (exact flag list), Xvfb, Browser, Page (rebindable backing), isolated World, selectors, auto-wait actions, input, network hooks, cookies, MutationObserver
  surf-runtime   implements surf_vm::Host: implicit browser/page, bare actions, NativeObject wrappers, handlers, spawn/parallel for, tasks, actors, supervisors, lifetime rule
  surf-cli       `surf run | check | doctor | repl` — tokio current_thread + LocalSet, exit codes, diagnostics rendering
protocol/        vendored browser_protocol.json + js_protocol.json (+ VERSION) for codegen
docs/            this file, language.md
examples/        hello, login, two-tabs, scrape-emit, parallel-pool, supervised
```

Dependency direction (strict, no cycles):

```
surf-cli → surf-runtime → surf-browser → surf-cdp
             ↓                 
           surf-vm → surf-syntax
```

`surf-syntax` and `surf-vm` have **no** dependency on `tokio`,
`std::process` or `std::fs` and build for `wasm32-unknown-unknown`
(Decision 8). `surf-vm` uses `futures::future::LocalBoxFuture` only.

## Data flow

```
script.surf
   │  surf_syntax::parse(name, src)            → ast::Program | Diagnostics
   ▼
surf-syntax ──► surf-vm ──► surf-runtime ──► surf-browser ──► surf-cdp ──► transport ──► Chrome
  lexer         compile()    Host impl         Page/World       Session       pipe fd3/fd4
  parser        Vm::run()    bare actions      auto-wait        call/events   (or ws)
  diagnostics   Value        pages registry    Input.*          \0 framing
                             handlers/tasks    createIsolatedWorld
```

1. **surf-cli** reads the file, builds a `tokio` `current_thread` runtime and
   a `LocalSet`, and calls `surf_runtime::run_source`.
2. **surf-syntax** lexes (spaces-only indentation → `INDENT`/`DEDENT`,
   duration literals, interpolated strings) and parses into `ast::Program`.
   Errors are `Diagnostics` rendered with ariadne.
3. **surf-vm** compiles the AST to bytecode. Top-level declarations
   (`browser:`, `fn`, `task`, `actor`, `supervisor`, `on …:`) become
   `Op::Declare` instructions in the main chunk's prologue, so the host sees
   every declaration before the first statement executes. `Vm::run` executes;
   anything that blocks is an `async` call into the `Host`.
4. **surf-runtime** implements `Host`. It turns `browser:` props into
   `LaunchOptions` (nothing launches yet), resolves bare actions to the sole
   page (auto-launching the browser and creating the page on first use),
   wraps browser objects as `NativeObject`s, and runs handlers / spawned
   tasks / actors / supervisors on the `LocalSet` with forked `Vm`s.
   `resolve_global("page")` is synchronous, so it returns a *lazy* page
   handle that binds to a real `Page` on its first (async) method call.
5. **surf-browser** owns the quiet policy: exact launch flags, pipe
   creation, isolated worlds, auto-waiting, input synthesis, ref-counted
   domain enables. It is the only crate that knows CDP method names.
6. **surf-cdp** moves JSON frames: ids, `sessionId` routing, per-method
   broadcast event channels, typed commands for an allow-list of domains.
7. **Transport**: pipe by default (Chrome reads fd 3, writes fd 4; each
   message is UTF-8 JSON + one `\0`). WebSocket only for `cdp: 9222`,
   `cdp: "ws://…"`, `pool: "wss://…"`.

## Quiet rules (where enforced)

| rule | enforced in |
|------|-------------|
| never `Runtime.enable`, never `DOM.enable` | `surf_cdp::FORBIDDEN_METHODS` (debug assert in `Session::call_raw`), `Session::enable_domain` rejects `Runtime`/`DOM`; `surf-browser` only creates isolated worlds |
| exact launch flags, no `--enable-automation`, no `--disable-*` | `surf_browser::launch::LaunchOptions::args` (unit-tested) |
| only `Page.enable` by default; `Network`/`Fetch` only while a hook exists | `surf_cdp::DomainGuard` ref-counting, owned by `surf_browser::network` hooks |
| pipe by default, no listening port unless `cdp: 9222` | `surf_browser::launch::CdpMode` |
| no main-world injection; helpers in the isolated world under random names | `surf_browser::world`, `surf_browser::observer` |

## Public contracts

These signatures are the agreed shapes; implementers keep them. They exist
as compiling skeletons in the crates.

### surf-cdp (`Send + Sync`; `tokio::sync` internally)

```rust
pub trait Transport: Send {
    fn send(&mut self, frame: &[u8]) -> BoxFuture<'_, io::Result<()>>;
    fn recv(&mut self) -> BoxFuture<'_, io::Result<Option<Vec<u8>>>>;
}

pub struct Connection;
impl Connection {
    pub fn new(t: Box<dyn Transport>) -> Arc<Connection>;
    pub fn root(&self) -> Session;
    pub async fn attach(&self, target_id: &str) -> Result<Session, CdpError>;
    pub fn set_trace(&self, on: bool);
    pub fn is_closed(&self) -> bool;
}

pub struct Session;
impl Session {
    pub async fn call<P: Serialize, R: DeserializeOwned>(&self, method: &str, params: P) -> Result<R, CdpError>;
    pub async fn call_raw(&self, method: &str, params: serde_json::Value) -> Result<serde_json::Value, CdpError>;
    pub async fn send<C: Command>(&self, c: C) -> Result<C::Response, CdpError>;
    pub fn events(&self, method: &str) -> tokio::sync::broadcast::Receiver<Event>;
    pub async fn enable_domain(&self, domain: &str) -> Result<DomainGuard, CdpError>;
    pub fn session_id(&self) -> Option<&str>;
    pub async fn detach(&self) -> Result<(), CdpError>;
}

pub struct Event { pub method: String, pub params: serde_json::Value, pub session_id: Option<String> }

pub enum CdpError {
    Protocol { code: i64, message: String, method: String },
    Closed,
    BrowserCrashed { exit_code: Option<i32>, stderr_tail: String },
    Io(std::io::Error),
    Json(serde_json::Error),
    Timeout { method: String },
}

pub trait Command: Serialize { const METHOD: &'static str; type Response: DeserializeOwned; }
```

`Connection::root()` / `attach()` take `self: &Arc<Self>` in the skeleton
(they must hand the session an `Arc`); call them through the `Arc` returned
by `new`.

### surf-vm (single-threaded; wasm32-clean; `futures::future::LocalBoxFuture`)

```rust
pub enum Value {
    Nil, Bool(bool), Int(i64), Float(f64), Str(Rc<str>),
    List(Rc<RefCell<Vec<Value>>>), Map(Rc<RefCell<IndexMap<Rc<str>, Value>>>),
    Duration(std::time::Duration), Fn(Rc<Closure>), Native(Rc<dyn NativeObject>),
}

pub struct Args { pub positional: Vec<Value>, pub kwargs: IndexMap<Rc<str>, Value> }

pub trait NativeObject {
    fn type_name(&self) -> &str;
    fn call_method<'a>(&'a self, vm: &'a mut Vm, name: &str, args: Args) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;
    fn get_prop(&self, name: &str) -> Option<Value> { None }
    fn to_json(&self) -> Option<serde_json::Value> { None }
}

pub trait Host {
    fn declare(&self, decl: Declaration);
    fn resolve_global(&self, name: &str) -> Option<Value>;
    fn call_global<'a>(&'a self, vm: &'a mut Vm, name: &str, args: Args) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;
    fn spawn<'a>(&'a self, vm: &'a mut Vm, name: &str, args: Args) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;
    fn parallel_for<'a>(&'a self, vm: &'a mut Vm, items: Value, body: Rc<Closure>, opts: Args) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;
    fn sleep<'a>(&'a self, d: Duration) -> LocalBoxFuture<'a, ()>;
    fn print(&self, s: &str);
    fn emit(&self, v: &Value);
    fn fs<'a>(&'a self, op: FsOp) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;
    fn env(&self, name: &str) -> Option<String>;
}

pub enum Prop { Const(Value), Lazy(Rc<Closure>) }   // task/actor/supervisor header props

pub enum Declaration {
    Config { name: String, alias: Option<String>, props: IndexMap<Rc<str>, Value> },
    Fn { name: String, closure: Rc<Closure> },
    Task { name: String, params: Vec<Rc<str>>, props: IndexMap<Rc<str>, Prop>, body: Rc<Closure> },
    Actor { name: String, params: Vec<Rc<str>>, props: IndexMap<Rc<str>, Prop>, body: Rc<Closure> },
    Supervisor { name: String, props: IndexMap<Rc<str>, Prop>, body: Rc<Closure> },
    Handler { event: String, args: Vec<Value>, body: Rc<Closure> },
}

pub struct RuntimeError {
    pub message: String,
    pub span: Option<surf_syntax::Span>,
    pub cdp_method: Option<String>,
    pub selector: Option<String>,
    pub cause: Option<Box<dyn std::error::Error>>,
}

pub struct Vm;
impl Vm {
    pub fn new(host: Rc<dyn Host>, globals: Rc<Globals>) -> Vm;
    pub fn fork(&self) -> Vm;                       // same host/globals/cancel token, fresh stack
    pub async fn run(&mut self, program: &CompiledProgram) -> Result<(), RuntimeError>;
    pub async fn call(&mut self, f: Rc<Closure>, args: Args) -> Result<Value, RuntimeError>;
    pub fn cancel_token(&self) -> CancelToken;
}

pub fn compile(name: &str, program: &surf_syntax::ast::Program) -> Result<CompiledProgram, CompileError>;
```

Property semantics: config props are evaluated eagerly at declaration time
(`env(...)` allowed). Task/actor/supervisor props are `Prop::Const` for
literals and `Prop::Lazy` thunks for anything that must be evaluated at use
time (`on_fail: shift_proxy()` runs once per failure, never at declaration).

### surf-browser: Page / backing indirection (Decision 10)

```rust
pub struct Backing { pub session: Session, pub target_id: String, pub browser_context_id: Option<String>, pub frame_id: String }
pub struct Migration { pub cookies: bool, pub storage: bool, pub url: bool }

#[derive(Clone)] pub struct Page(Rc<PageInner>);   // stable handle; clones share state
impl Page {
    pub fn index(&self) -> usize;
    pub fn backing(&self) -> Option<Backing>;
    pub async fn world(&self) -> Result<World, BrowserError>;
    pub async fn rebind(&self, new_backing: Backing, migration: Migration) -> Result<(), BrowserError>;
    pub async fn close(&self) -> Result<(), BrowserError>;
}
```

`rebind` replaces `{session, target_id, browser_context_id, frame_id}` under
the same handle, migrating cookies (`Storage.getCookies` / `setCookies`),
`localStorage`/`sessionStorage` (isolated-world eval) and the URL. One
mechanism serves supervisor restarts, `shift_proxy()`, and later per-page
Apostate personas (one process per persona; a logical `Browser` may own
several OS processes).

### Lifetime rule

A program stays alive while any handler block is registered or any task is
running. When the main body returns and nothing is pending, browsers Surf
launched are closed (temp profiles deleted, `profile:` dirs kept) and the
process exits 0. `exit` / `exit(n)` cancels every task via `CancelToken`,
closes everything, and exits with `n`. Attached browsers (`cdp: "ws://…"`,
`pool:`) are never closed.

## Threading model

`tokio` `current_thread` + `LocalSet`. VM values are `Rc`; `surf-browser`
and `surf-runtime` are `!Send`. `surf-cdp` is `Send + Sync` so that a
thread-per-core build can later shard connections across threads
(Decision 5). Thousands of concurrent CDP calls on one thread are fine —
scripts are IO-bound.

## Testing

- Unit tests per crate (`cargo test --workspace`).
- `crates/surf-cli/tests/e2e.rs` runs against a real Chrome: `SURF_CHROME`
  overrides discovery; when no Chrome is found the test prints a skip
  message and passes. It must actually run on developer Macs and in the CI
  `e2e` job.
- Fixture pages are served by an in-process `axum` server (dev-dependency).
- Codegen input: `protocol/*.json`, pinned in `protocol/VERSION`.
