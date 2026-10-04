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
  surf-browser   Chrome discovery, quiet launch (exact flag list, crash watcher, shutdown ladder), Xvfb (display.rs), Browser, Page (rebindable backing), isolated World, selectors, auto-wait actions, input, network hooks, cookies, MutationObserver
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
| never `Runtime.enable`, never `DOM.enable` | `surf_cdp::FORBIDDEN_METHODS` (refused by `Session::call_raw` in every build; no typed command is generated for them), `Session::enable_domain` rejects `Runtime`/`DOM`; `surf-browser` only creates isolated worlds |
| exact launch flags, no `--enable-automation`; the only `--disable-*` is `--disable-blink-features=AutomationControlled`, on launched browsers only (Decision 12) | `surf_browser::launch::LaunchConfig::args` (unit-tested; `tests/launch.rs` checks `navigator.webdriver === false`, no infobar, and that the switch is why) |
| only `Page.enable` by default; `Network`/`Fetch` only while a hook exists | `surf_cdp::DomainGuard` ref-counting, owned by `surf_browser::network` hooks |
| pipe by default, no listening port unless `cdp: 9222` | `surf_browser::launch::CdpMode` → `TransportChoice` (`tests/launch.rs` runs `lsof -iTCP -sTCP:LISTEN -p <pid>`) |
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

`Connection::root()` / `attach()` take `self: &Arc<Self>` (they hand the
session an `Arc`); call them through the `Arc` returned by `new`. Additive
helpers beyond the contract: `Connection::session(id)` (wrap a `sessionId`
learned from `Target.attachedToTarget`), `close()`, `wait_closed()`,
`mark_crashed(exit_code, stderr_tail)` (the launcher's process watcher
calls it; in-flight and later calls then fail with
`CdpError::BrowserCrashed` instead of `Closed`),
`Session::with_timeout(Option<Duration>)`, `Session::domain_refcount`,
`Event::parse::<E: ProtocolEvent>()`, `surf_cdp::event::next(&mut rx)`
(drains a receiver, logging lag).

#### Transport and session model

```
  Session (root)      Session ("S1")       Session ("S2")        ← cheap clones, Arc<Connection>
      │ call(method, params)   │ events("Page.loadEventFired")
      ▼                        ▼
  ┌──────────────────────── Connection ────────────────────────┐
  │ next_id: AtomicU64       pending: id → oneshot<RawValue>    │
  │ routes: (sessionId?, method) → broadcast::Sender<Event>(1024)│
  │ domain_counts: (sessionId?, domain) → live DomainGuards     │
  └──────────────┬──────────────────────────────▲──────────────┘
   outgoing mpsc │                              │ dispatch()
                 ▼                              │
          ┌──── driver task (one per connection) ────┐
          │ select! { outgoing.recv() → transport.send │
          │           transport.recv() → dispatch }    │   recv must be cancel-safe
          └───────────────────┬──────────────────────┘
                              ▼
                   Box<dyn Transport>   pipe (fd 3 / fd 4)  |  websocket
```

* **Ids** are connection-wide and monotonic. A response is matched by `id`
  only; the `sessionId` echoed on responses is ignored.
* **Events** are routed by `(sessionId, method)`; `None` is the root. Each
  `(session, method)` has one `broadcast` channel of capacity 1024 created
  on first `events()`; `events("*")` is a per-session catch-all. When a
  channel is full the sender logs `warn!` (a slow subscriber will see
  `Lagged(n)`); `surf_cdp::event::next` logs and skips the lag. Channels of
  a session are dropped on `Session::detach()` or when Chrome reports
  `Target.detachedFromTarget` for it, so receivers end with `Closed`.
* **Sessions**: `attach(target_id)` sends
  `Target.attachToTarget{targetId, flatten:true}`; every later command on
  that `Session` carries `"sessionId"`. `detach()` sends
  `Target.detachFromTarget{sessionId}` on the root.
* **Domains**: `enable_domain("Page")` returns a `DomainGuard`. The first
  guard per `(session, domain)` sends `Page.enable`; dropping the last
  spawns a best-effort `Page.disable` (re-checked under a lock so a guard
  taken in between cancels the disable). `Runtime` / `DOM` are refused.
* **Lifetime**: the driver holds a `Weak<Connection>`. Dropping the last
  `Session`/`Arc<Connection>` (or `close()`) ends the driver, which drops
  the transport — closing the pipe / websocket. Transport EOF or error
  marks the connection closed: pending calls fail with `CdpError::Closed`,
  every event channel closes, `wait_closed()` resolves.
* **Timeouts**: none by default; `with_timeout` yields a handle whose calls
  fail with `CdpError::Timeout{method}`. The pending slot is removed when a
  caller gives up, so a late answer is ignored.
* **Tracing**: `set_trace(true)` or `SURF_TRACE_CDP=1` logs every frame at
  `debug` under target `surf_cdp::trace`, `→` sent / `←` received,
  truncated at 4 KiB.

#### Pipe framing (`--remote-debugging-pipe`)

Two anonymous pipes. Chrome reads commands from **fd 3** and writes
responses/events to **fd 4**:

```
  parent ──pipe A──▶ child fd 3      parent write end  = PipeTransport.writer
  parent ◀──pipe B── child fd 4      parent read end   = PipeTransport.reader
```

Each message is the UTF-8 JSON text followed by exactly one `0x00` byte
(ASCIIZ framing); there is no length prefix and JSON never contains a raw
NUL, so the reader scans for `0x00`:

```
  7B 22 69 64 22 3A 31 2C 22 6D 65 74 68 6F 64 22 3A ... 7D 00 7B 22 69 64 22 3A 32 ...
  {  "  i  d  "  :  1  ,  "  m  e  t  h  o  d  "  :     }  \0 {  "  i  d  "  :  2
```

`FrameDecoder` buffers across reads, so one `read` may yield zero, one or
several frames and a frame may span many reads. `pipe::create_pair()`
returns `(PipeTransport, ChildFds)`; on unix both child fds are `CLOEXEC`
and blocking — the launcher `dup2`s them onto 3 and 4 in `pre_exec`
(`dup2` clears `CLOEXEC` on the new descriptor only) and drops `ChildFds`
after `spawn` so the parent sees EOF when Chrome exits. On Windows the two
handles are made inheritable and passed as
`--remote-debugging-io-pipes=<read>,<write>` (decimal handle values);
parent ends are `tokio::fs::File`s (anonymous pipes have no overlapped IO).
`--remote-debugging-pipe=cbor` exists and is not used (TASKS.md).

#### WebSocket (`cdp: 9222`, `cdp: "ws://…"`, `pool: "wss://…"`)

One CDP message per text frame; message/frame size limits are lifted
(screenshots). `transport::tcp::connect(host, port)` does one raw
`GET /json/version` (no HTTP client dependency; `Host` must be an IP or
`localhost` or Chrome refuses), reads `webSocketDebuggerUrl` and connects.
A peer that disappears without the closing handshake is treated as EOF.

#### Codegen (`crates/surf-cdp/build.rs`)

Runs at build time over `protocol/*.json` for the allow-list `Target, Page,
Runtime, DOM, Input, Network, Fetch, Emulation, Browser, Storage, IO,
Security` and writes `$OUT_DIR/protocol.rs`, included as
`surf_cdp::protocol::{target, page, …}`. `Domain.fooBar` →
`domain::FooBar` (`Serialize`, `impl Command { METHOD, Response }`) +
`domain::FooBarResponse` (`Deserialize`; `Empty` when nothing is returned);
events → `domain::FooBarEvent` (`impl ProtocolEvent { METHOD }`). Named
string enums become Rust enums with a `#[serde(other)] Unrecognized`
variant; inline enums are `String`; `any` / opaque objects / refs outside
the allow-list are `serde_json::Value`; self-referential structs are
`Box`ed. `Runtime.enable` and `DOM.enable` are skipped.

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
    fn as_any(&self) -> Option<&dyn Any> { None }          // downcast hook (ranges, builtin fns)
    fn iter_items(&self) -> Option<Vec<Value>> { None }    // `for x in obj:` / `len(obj)`
}
// Calling a native value (`obj(args)`) invokes `call_method(vm, "__call__", args)`.

pub trait Host {
    fn declare(&self, decl: Declaration);
    fn resolve_global(&self, name: &str) -> Option<Value>;
    fn call_global<'a>(&'a self, vm: &'a mut Vm, name: &str, args: Args) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;
    fn spawn<'a>(&'a self, vm: &'a mut Vm, name: &str, args: Args) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;
    fn spawn_method<'a>(&'a self, vm: &'a mut Vm, receiver: Value, name: &str, args: Args) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> { /* default: error */ }
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

impl RuntimeError {
    pub fn exit(code: i32) -> Self;        // `exit` travels as an error with an `ExitRequest` cause
    pub fn cancelled() -> Self;            // cooperative cancellation, `Cancelled` cause
    pub fn exit_code(&self) -> Option<i32>;
    pub fn is_catchable(&self) -> bool;    // false for exit / cancellation
    pub fn to_diagnostic(&self) -> surf_syntax::Diagnostic;
    pub fn to_value(&self, line: Option<u32>) -> Value;   // the `catch e:` map
}

pub struct Vm;
impl Vm {
    pub fn new(host: Rc<dyn Host>, globals: Rc<Globals>) -> Vm;
    pub fn fork(&self) -> Vm;                       // same host/globals/cancel token, fresh stack
    pub async fn run(&mut self, program: &CompiledProgram) -> Result<(), RuntimeError>;
    pub async fn call(&mut self, f: Rc<Closure>, args: Args) -> Result<Value, RuntimeError>;
    pub fn call_value<'a>(&'a mut self, callee: Value, args: Args) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;
    pub fn cancel_token(&self) -> CancelToken;
}

pub fn compile(name: &str, program: &surf_syntax::ast::Program) -> Result<CompiledProgram, CompileError>;
pub fn compile_with_source(name: &str, source: &str, program: &Program) -> Result<CompiledProgram, CompileError>; // keeps a line table for `e.line`
```

Property semantics: config props are evaluated eagerly at declaration time
(`env(...)` allowed). Task/actor/supervisor props are `Prop::Const` for
literals and bare symbols (`strategy: one_for_one`) and `Prop::Lazy` thunks
for anything that must be evaluated at use time (`on_fail: shift_proxy()`
runs once per failure, never at declaration).

#### VM design notes (task 6)

- **Stack machine, one loop.** `Op` is a small `Copy` enum; constants live
  in a per-chunk pool. Script→script calls push a frame and continue in the
  same loop (no Rust recursion). Every native call (`Host::call_global`,
  `NativeObject::call_method`, stdlib) returns a `LocalBoxFuture` the loop
  awaits in place; natives call back into script code through `Vm::call`,
  which runs a nested loop on the same stack. The boxed future is what
  breaks the async-recursion cycle.
- **Global resolution order** for a bare name `f(...)` that is not a local
  or upvalue: `Host::resolve_global(f)` (user `fn`s, `page`, `browser`,
  `self`, task/actor names) → stdlib `Globals` → `Host::call_global(f)`
  (bare browser actions). So the host can shadow a stdlib name, and
  unknown names reach the host without the VM materialising a value.
- **Keyword arguments — trailing kwargs slot.** The call convention pushes
  the callee, then the positional arguments, then one `(name, value)` pair
  per keyword argument (the name as a string constant); `Call`,
  `CallGlobal`, `CallMethod`, `Spawn` carry both counts. Keywords therefore
  always *trail* the positionals on the stack and are popped into
  `Args.kwargs` in source order. For script closures, keywords bind by
  parameter name (unknown keyword / argument given twice / missing required
  parameter are runtime errors naming the parameter); defaults are
  evaluated by the callee's prologue (`ParamDefault`) only for parameters
  the caller did not supply. Natives receive `Args` and validate keywords
  themselves (`Args::check_kwargs`).
- **Scoping.** Locals are function-scoped; a cheap pre-scan of each body
  collects every assigned name so the slot exists before the first
  assignment regardless of position. A nested function assigning a name
  that exists in an enclosing function updates it (no `nonlocal`); a name
  that exists nowhere becomes a local of the nested function. Captured
  variables are *boxed on capture*: the slot is upgraded to an
  `Rc<RefCell<Value>>` when a closure captures it, so both sides see every
  assignment. Hoisted items (`fn`, `task`, `actor`, `supervisor`, `on`,
  `browser:`) are compiled as closures of `<main>` (they can read top-level
  variables) and delivered via `Op::Declare` → `Host::declare` before the
  first statement runs.
- **try/catch** is a handler stack (`TryBegin`/`TryEnd`) scoped to the
  current `Vm::call`; on an error the VM unwinds frames/stack/iterators to
  the handler and pushes `RuntimeError::to_value` (`message`, `selector`,
  `cdp_method`, `line`). `exit(n)` and cancellation are errors with marker
  causes that no handler catches. `break`/`continue`/`return` emit the
  `TryEnd`s (and `IterEnd`) needed to leave the regions they cross.
- **Cancellation** is checked at loop back-edges (`Loop`) and before every
  native call; `CancelToken` is shared by `Vm::fork`.
- **Ranges** are a `Native` (`surf_vm::Range`, lazy; `len`, indexing and
  iteration work without materialising). `json` and `fs` are native
  objects registered in `Globals`; stdlib functions referenced as values
  (`f = len`) are `BuiltinFn` natives.
- **Cold start** (parse + compile + run of a 20-line script) is asserted
  < 20 ms in a debug build by `crates/surf-vm/tests/fixtures.rs`.

### surf-browser: discovery and launch

```rust
pub fn discovery::find_chrome(explicit: Option<&Path>) -> Result<Found { path, version: Option<String>, origin }, BrowserError>;
pub fn discovery::chrome_or_skip(test_name: &str) -> Option<PathBuf>;   // tests: prints a skip message

pub struct LaunchOptions { path, cdp: CdpMode, headless: Option<bool>, virtual_display, size, profile, proxy, proxies, flags, timeout, engine }  // the `browser:` block
impl LaunchOptions { pub fn resolve(&self) -> Result<LaunchConfig, BrowserError>; }        // discovery + headed/headless decision + proxy parse

pub struct LaunchConfig { path, headless: bool, size, proxy: Option<ProxySpec>, profile, flags, env, virtual_display, transport: TransportChoice { Pipe | Port(u16) }, correct_automation_controlled }
impl LaunchConfig { pub fn args(&self, user_data_dir: &Path) -> Vec<String>; }           // THE flag list

pub struct ProxySpec { scheme, host, port, credentials: Option<Credentials> }             // ProxySpec::parse("http://u:p@host:8080" | "socks5://host:1080" | "host:8080")

pub async fn launch(cfg: LaunchConfig) -> Result<Launched, BrowserError>;
pub struct Launched { connection: Arc<Connection>, process: Process, profile_dir: ProfileDir, display: Option<VirtualDisplay>, proxy_credentials: Option<Credentials>, product, path, args }
impl Launched { pub fn root(&self) -> Session; pub fn stderr_tail(&self) -> String; pub async fn close(&self); }
```

Discovery order: `browser.path` → `SURF_CHROME` (both hard errors when set
but missing) → `~/.cache/surf/chrome/**` (`surf install`) → platform
locations (macOS app bundles, `/opt/google/chrome/chrome`, `PATH` names,
Windows Program Files / LocalAppData) → Playwright / Puppeteer / Apostate
caches via a bounded walk for an executable named like a Chromium binary
(newest version first). Version via `--version` with a 3 s timeout. The
`NotFound` error lists every location tried.

Launch: a `surf-profile-*` temp dir (removed on close) or the user's
`profile:`; Xvfb on Linux when `virtual: true` and no `DISPLAY`
(`display.rs`); pipe pair from `surf_cdp::transport::pipe::create_pair`
installed on fd 3 / fd 4 in `pre_exec` (`launch::sys`, the crate's only
`unsafe`; fds already at 3/4 are moved first); stderr drained into a
64-line ring buffer; first `Browser.getVersion` within 10 s or the error
carries the stderr tail. Port mode (`cdp: N`, `0` = Chrome picks, read from
`DevToolsActivePort`) connects through `/json/version`. A watcher task owns
the `Child`; an unexpected exit calls `Connection::mark_crashed` so calls
fail with `BrowserCrashed { exit_code, stderr_tail }` (the transport
wrapper delays EOF up to 1 s so in-flight calls see the crash, not
`Closed`). `Launched::close()` (idempotent; the Ctrl-C handler calls it):
`Browser.close` → 2 s → `SIGTERM` → 2 s → `SIGKILL`, then remove the temp
profile (retrying while helper processes drain) and stop Xvfb. Dropping a
`Launched` without `close()` kills the process and removes the temp
profile best-effort.

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
