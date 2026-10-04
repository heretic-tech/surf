//! Tasks: `spawn`, `parallel for`, `task` / `actor` properties, handles.
//!
//! Every concurrent unit — a spawned `fn`, `task` or `actor`, and each
//! `parallel for` item — is a [`TaskInfo`] registered with the runtime and
//! run on its own tokio local task by [`spawn_task`]:
//!
//! 1. a fresh [`Vm`] (own cancel token) shares the program's `Globals`;
//! 2. [`TASK_CTX`] gives the body a private page on its first bare action
//!    (`crate::pages`); [`CURRENT_TASK`] gives it `self`, a mailbox and
//!    `receive()`;
//! 3. the body is raced against the task's own cancellation
//!    (`handle.cancel()`, `fail_fast`, supervisor `one_for_all`) and the
//!    program's exit signal;
//! 4. an actor whose body registered `on message:` handlers stays alive
//!    (mailbox open) until cancelled;
//! 5. the private page is closed (supervised children keep theirs for the
//!    restart), the result is stored for `join()` / `parallel for` /
//!    the supervisor, and the lifetime counter is decremented.
//!
//! `task` / `actor` bodies run through [`run_decl`], which applies
//! `retry`, `on_fail`, `timeout` and `fresh` — the same code path whether
//! the task was spawned or called synchronously (`fetch(url)`).

use crate::actors::Mailbox;
use crate::host::Runtime;
use crate::pages::{current_ctx, TaskCtx, TaskKind, TASK_CTX};
use crate::supervisors::{current_supervising, SupervisorRun, SUPERVISING};
use futures::future::{Either, LocalBoxFuture};
use indexmap::IndexMap;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;
use surf_browser::{Browser, Migration, Page, RebindTarget};
use surf_vm::{
    format_duration, Args, CancelToken, Closure, NativeObject, Prop, RuntimeError, Value, Vm,
};
use tokio::sync::{Notify, Semaphore};

/// `task name(params):` / `actor Name(params):` as declared.
pub struct TaskDecl {
    /// Declared name.
    pub name: String,
    /// `actor` (own mailbox semantics) rather than `task`.
    pub actor: bool,
    /// Parameter names.
    pub params: Vec<Rc<str>>,
    /// `retry`, `on_fail`, `timeout`, `fresh`.
    pub props: IndexMap<Rc<str>, Prop>,
    /// Body closure.
    pub body: Rc<Closure>,
}

impl TaskDecl {
    /// `task` or `actor`.
    pub fn kind(&self) -> &'static str {
        if self.actor {
            "actor"
        } else {
            "task"
        }
    }
}

/// What a task runs.
#[derive(Clone)]
pub enum Callee {
    /// A plain `fn`: one run, no properties.
    Fn(Rc<Closure>),
    /// A `task` / `actor` declaration (properties applied per run).
    Decl(Rc<TaskDecl>),
    /// One `parallel for` item: `body(item)` once a permit is available.
    Item {
        /// The loop body (one parameter).
        body: Rc<Closure>,
        /// `limit:` permits.
        permits: Rc<Semaphore>,
    },
}

/// Flavour of a task (for messages and `type(h)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// `spawn f()` of a plain `fn`.
    Fn,
    /// `spawn t()` of a `task`.
    Task,
    /// `spawn A()` of an `actor`.
    Actor,
    /// A `parallel for` item.
    Item,
}

impl Flavor {
    fn noun(self) -> &'static str {
        match self {
            Flavor::Fn => "spawned fn",
            Flavor::Task => "task",
            Flavor::Actor => "actor",
            Flavor::Item => "parallel for item",
        }
    }
}

/// One live (or finished) task.
pub struct TaskInfo {
    /// Unique id (`self.id`, `h.id`), 1-based in spawn order.
    pub id: u64,
    /// Declared / function name (`self.name`).
    pub name: String,
    /// Flavour.
    pub flavor: Flavor,
    /// Page binding for bare actions.
    pub ctx: Rc<TaskCtx>,
    /// Incoming messages.
    pub mailbox: Mailbox,
    /// `on message:` handlers registered by the body.
    pub message_handlers: RefCell<Vec<Rc<Closure>>>,
    /// Supervisor tree the task belongs to (`broadcast` scope).
    pub group: Cell<Option<u64>>,
    /// A supervisor handles this task's failures (no stderr report).
    pub supervised: Cell<bool>,
    /// Do not close the private page when the body ends (the supervisor
    /// reuses it for the restart).
    pub keep_page: Cell<bool>,
    cancel: CancelToken,
    cancelled: Cell<bool>,
    result: RefCell<Option<Result<Value, Rc<RuntimeError>>>>,
    notify: Notify,
    joiners: Cell<usize>,
}

impl TaskInfo {
    /// `task fetch#3`.
    pub fn label(&self) -> String {
        format!("{} {}#{}", self.flavor.noun(), self.name, self.id)
    }

    /// Whether the body has finished.
    pub fn is_done(&self) -> bool {
        self.result.borrow().is_some()
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.get()
    }

    /// Stop the task: the VM stops at its next check, a pending await is
    /// dropped, the private page is closed.
    pub fn cancel(&self) {
        self.cancelled.set(true);
        self.cancel.cancel();
        self.notify.notify_waiters();
    }

    async fn wait_cancel(&self) {
        loop {
            let notified = self.notify.notified();
            if self.cancelled.get() {
                return;
            }
            notified.await;
        }
    }

    /// Wait for the result; errors are re-raised as copies.
    pub async fn wait(&self) -> Result<Value, RuntimeError> {
        self.joiners.set(self.joiners.get() + 1);
        let r = loop {
            let notified = self.notify.notified();
            if let Some(r) = self.result.borrow().as_ref() {
                break match r {
                    Ok(v) => Ok(v.clone()),
                    Err(e) => Err(copy_error(e)),
                };
            }
            notified.await;
        };
        self.joiners.set(self.joiners.get() - 1);
        r
    }

    /// The stored result, if finished.
    pub fn result(&self) -> Option<Result<Value, RuntimeError>> {
        self.result.borrow().as_ref().map(|r| match r {
            Ok(v) => Ok(v.clone()),
            Err(e) => Err(copy_error(e)),
        })
    }

    fn finish(&self, rt: &Runtime, r: Result<Value, RuntimeError>) {
        let r = r.map_err(Rc::new);
        if let Err(e) = &r {
            if let Some(code) = e.exit_code() {
                rt.lifetime().request_exit(code);
            } else if !e.is_cancelled()
                && self.joiners.get() == 0
                && !self.supervised.get()
                && self.flavor != Flavor::Item
            {
                rt.report_failure(&self.label(), e);
            }
        }
        *self.result.borrow_mut() = Some(r);
        self.notify.notify_waiters();
    }
}

tokio::task_local! {
    /// The task the running code belongs to (absent in the main body and
    /// in handler bodies that are not inside an actor).
    pub static CURRENT_TASK: Rc<TaskInfo>;
}

/// The current task, if any.
pub fn current_task() -> Option<Rc<TaskInfo>> {
    CURRENT_TASK.try_with(|t| t.clone()).ok()
}

/// An error stored by a task and re-raised to joiners.
#[derive(Debug)]
struct Shared(Rc<RuntimeError>);

impl std::fmt::Display for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for Shared {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.cause.as_deref()
    }
}

/// A copy of a stored error that keeps its message, span, selector and CDP
/// method. `exit` stays an exit request; a task's cancellation becomes an
/// ordinary, catchable `cancelled` error for whoever joins it (see
/// [`was_cancelled`]).
pub fn copy_error(e: &Rc<RuntimeError>) -> RuntimeError {
    if let Some(code) = e.exit_code() {
        return RuntimeError::exit(code);
    }
    let mut r = RuntimeError::new(e.message.clone()).with_cause(Shared(e.clone()));
    r.span = e.span;
    r.selector = e.selector.clone();
    r.cdp_method = e.cdp_method.clone();
    r
}

/// Whether a joined task's error says it was cancelled.
pub fn was_cancelled(e: &RuntimeError) -> bool {
    e.is_cancelled()
        || e.cause
            .as_deref()
            .and_then(|c| c.downcast_ref::<Shared>())
            .is_some_and(|s| s.0.is_cancelled())
}

// ───────────────────────── spawning ─────────────────────────

/// Start `callee(args)` on its own task. `page` hands over an existing
/// private page (supervisor restart); `supervising` makes `spawn` calls
/// inside the body register as that supervisor's children (`parallel
/// for` items of a supervisor body).
pub fn spawn_task(
    rt: &Rc<Runtime>,
    callee: Callee,
    args: Args,
    name: &str,
    flavor: Flavor,
    page: Option<(Rc<Browser>, Page)>,
    supervising: Option<Rc<SupervisorRun>>,
) -> Rc<TaskInfo> {
    let id = rt.next_task_id();
    let label = format!("{name}#{id}");
    let ctx = match page {
        Some((b, p)) => TaskCtx::spawned_with(&label, b, p),
        None => TaskCtx::spawned(&label, false),
    };
    let vm = rt.task_vm();
    let info = Rc::new(TaskInfo {
        id,
        name: name.to_owned(),
        flavor,
        ctx: ctx.clone(),
        mailbox: Mailbox::default(),
        message_handlers: RefCell::new(Vec::new()),
        group: Cell::new(current_task().and_then(|t| t.group.get())),
        supervised: Cell::new(false),
        keep_page: Cell::new(false),
        cancel: vm.cancel_token(),
        cancelled: Cell::new(false),
        result: RefCell::new(None),
        notify: Notify::new(),
        joiners: Cell::new(0),
    });
    rt.register_task(info.clone());
    rt.lifetime().task_started();
    let rt2 = rt.clone();
    let info2 = info.clone();
    tokio::task::spawn_local(async move {
        run_task(rt2, info2, vm, callee, args, supervising).await;
    });
    info
}

async fn run_task(
    rt: Rc<Runtime>,
    info: Rc<TaskInfo>,
    mut vm: Vm,
    callee: Callee,
    args: Args,
    supervising: Option<Rc<SupervisorRun>>,
) {
    let ctx = info.ctx.clone();
    let body = async {
        match callee {
            Callee::Fn(f) => vm.call(f, args).await,
            Callee::Decl(d) => run_decl(&rt, &d, args, &mut vm).await,
            Callee::Item { body, permits } => {
                let _permit = permits.acquire().await;
                vm.call(body, args).await
            }
        }
    };
    let body = match supervising {
        Some(s) => Either::Left(SUPERVISING.scope(s, body)),
        None => Either::Right(body),
    };
    let scoped = TASK_CTX.scope(ctx.clone(), CURRENT_TASK.scope(info.clone(), body));
    let r = tokio::select! {
        r = scoped => r,
        _ = info.wait_cancel() => Err(RuntimeError::cancelled()),
        _ = rt.lifetime().exit_signal() => Err(RuntimeError::cancelled()),
    };
    if r.is_ok() && !info.message_handlers.borrow().is_empty() {
        // An actor with `on message:` handlers lives until cancelled.
        tokio::select! {
            _ = info.wait_cancel() => {},
            _ = rt.lifetime().exit_signal() => {},
        }
    }
    if !info.keep_page.get() {
        rt.release_private_page(&ctx).await;
    }
    rt.unregister_task(info.id);
    info.finish(&rt, r);
    rt.lifetime().task_finished();
}

/// `on message:` delivery: run `handler(msg)` on its own task, bound to
/// the actor's page and identity.
pub fn dispatch_message(rt: &Rc<Runtime>, task: &Rc<TaskInfo>, handler: Rc<Closure>, msg: Value) {
    rt.lifetime().task_started();
    let rt = rt.clone();
    let task = task.clone();
    tokio::task::spawn_local(async move {
        let mut vm = rt.vm();
        let fut = vm.call(handler, Args::positional(vec![msg]));
        let r = TASK_CTX
            .scope(task.ctx.clone(), CURRENT_TASK.scope(task.clone(), fut))
            .await;
        rt.task_result("on message", r);
        rt.lifetime().task_finished();
    });
}

/// `spawn name(args)`: a user `fn`, `task` or `actor` on its own task.
/// Inside a supervisor body the task becomes a supervised child.
pub async fn spawn(
    rt: &Rc<Runtime>,
    vm: &mut Vm,
    name: &str,
    args: Args,
) -> Result<Value, RuntimeError> {
    let _ = vm;
    let (callee, flavor) = if let Some(f) = rt.user_fn(name) {
        (Callee::Fn(f), Flavor::Fn)
    } else if let Some(d) = rt.task_decl(name) {
        let flavor = if d.actor { Flavor::Actor } else { Flavor::Task };
        (Callee::Decl(d), flavor)
    } else if rt.supervisor(name).is_some() {
        return Err(RuntimeError::new(format!(
            "supervisor {name} starts by itself — use {name}.join() or {name}.stop()"
        )));
    } else if crate::builtins::bare_action_name(name).is_some() || vm.globals().get(name).is_some()
    {
        return Err(RuntimeError::new(format!(
            "spawn {name}(…): `{name}` is a builtin — wrap it in a fn: `fn go(): {name}(…)` then `spawn go()`"
        )));
    } else {
        let names = rt.callable_names();
        let candidates: Vec<&str> = names.iter().map(String::as_str).collect();
        return Err(RuntimeError::new(format!(
            "spawn {name}(…): no fn, task or actor named `{name}`{}",
            crate::methods::did_you_mean(name, &candidates)
        )));
    };
    let info = spawn_task(rt, callee.clone(), args.clone(), name, flavor, None, None);
    if let Some(sup) = current_supervising() {
        sup.adopt(rt, callee, args, name, flavor, info.clone());
    }
    Ok(TaskHandle::value(rt, info))
}

// ───────────────────────── task / actor bodies ─────────────────────────

struct TaskProps {
    retry: u32,
    timeout: Option<Duration>,
    fresh: bool,
    on_fail: Option<Prop>,
}

async fn task_props(decl: &TaskDecl, vm: &mut Vm) -> Result<TaskProps, RuntimeError> {
    let mut p = TaskProps {
        retry: 0,
        timeout: None,
        fresh: true,
        on_fail: None,
    };
    let what = |k: &str| format!("{} {}: {k}:", decl.kind(), decl.name);
    for (k, prop) in &decl.props {
        match &**k {
            "on_fail" => p.on_fail = Some(prop.clone()),
            "retry" => match prop.resolve(vm).await? {
                Value::Int(n) if n >= 0 => p.retry = n as u32,
                v => {
                    return Err(RuntimeError::new(format!(
                        "{} expected a non-negative int, got {}",
                        what("retry"),
                        v.type_name()
                    )))
                }
            },
            "timeout" => match prop.resolve(vm).await? {
                Value::Duration(d) => p.timeout = Some(d),
                Value::Nil => p.timeout = None,
                v => {
                    return Err(RuntimeError::new(format!(
                        "{} expected a duration such as 60s, got {}",
                        what("timeout"),
                        v.type_name()
                    )))
                }
            },
            "fresh" => match prop.resolve(vm).await? {
                Value::Bool(b) => p.fresh = b,
                v => {
                    return Err(RuntimeError::new(format!(
                        "{} expected true or false, got {}",
                        what("fresh"),
                        v.type_name()
                    )))
                }
            },
            _ => {}
        }
    }
    Ok(p)
}

/// Run a `task` / `actor` body with its properties: up to `retry + 1`
/// attempts, each on a forked VM and (with `timeout:`) under a deadline;
/// between attempts the page is moved to a clean context when `fresh`
/// (default) and `on_fail` is evaluated. Non-catchable errors (`exit`,
/// cancellation) end the loop at once.
pub async fn run_decl(
    rt: &Rc<Runtime>,
    decl: &Rc<TaskDecl>,
    args: Args,
    vm: &mut Vm,
) -> Result<Value, RuntimeError> {
    let props = task_props(decl, vm).await?;
    if let Some(ctx) = current_ctx() {
        if ctx.kind == TaskKind::Spawned && ctx.page.borrow().is_none() {
            ctx.isolated.set(props.fresh);
        }
    }
    let attempts = props.retry + 1;
    let mut last: Option<RuntimeError> = None;
    for attempt in 1..=attempts {
        if attempt > 1 {
            if props.fresh {
                rt.fresh_page(&format!("{} {}", decl.kind(), decl.name))
                    .await?;
            }
            if let Some(p) = &props.on_fail {
                p.resolve(vm).await?;
            }
        }
        let mut attempt_vm = vm.fork();
        let fut = attempt_vm.call(decl.body.clone(), args.clone());
        let r = match props.timeout {
            Some(d) => match tokio::time::timeout(d, fut).await {
                Ok(r) => r,
                Err(_) => Err(RuntimeError::new(format!(
                    "{} {}: attempt {attempt} timed out after {}",
                    decl.kind(),
                    decl.name,
                    format_duration(&d)
                ))),
            },
            None => fut.await,
        };
        match r {
            Ok(v) => return Ok(v),
            Err(e) if !e.is_catchable() => return Err(e),
            Err(e) => {
                tracing::debug!(
                    "{} {}: attempt {attempt}/{attempts} failed: {}",
                    decl.kind(),
                    decl.name,
                    e.message
                );
                last = Some(e);
            }
        }
    }
    let mut e = last.unwrap_or_else(|| RuntimeError::new("task failed"));
    if attempts > 1 {
        e.message = format!(
            "{} ({} {}: gave up after {attempts} attempts)",
            e.message,
            decl.kind(),
            decl.name
        );
    }
    Err(e)
}

/// A `task` called like a function (`fetch(url)`): runs in the caller's
/// task with the task's properties. Actors must be spawned.
pub async fn call_decl(
    rt: &Rc<Runtime>,
    vm: &mut Vm,
    decl: Rc<TaskDecl>,
    args: Args,
) -> Result<Value, RuntimeError> {
    if decl.actor {
        return Err(RuntimeError::new(format!(
            "actor {0} runs on its own task — write `spawn {0}(…)`",
            decl.name
        )));
    }
    run_decl(rt, &decl, args, vm).await
}

/// `shift_proxy()`: move the current page to a new browser context behind
/// the next proxy of the default browser's `proxies:` list (round-robin;
/// cookies kept, URL and storage not). Returns the proxy URL.
pub async fn shift_proxy(rt: &Rc<Runtime>, args: Args) -> Result<Value, RuntimeError> {
    args.check_kwargs("shift_proxy", &[])?;
    let slot = rt.default_slot("shift_proxy")?;
    let proxies = slot.config.borrow().launch.proxies.clone();
    if proxies.is_empty() {
        return Err(RuntimeError::new(
            "shift_proxy(): no proxies configured — add `proxies: [\"http://…\", …]` to the browser: block",
        ));
    }
    let proxy = proxies[slot.next_proxy() % proxies.len()].clone();
    let (browser, page) = match rt.current_page() {
        Some(p) => p,
        None => rt.sole_page("shift_proxy").await?,
    };
    rt.rebind(
        &browser,
        &page,
        RebindTarget::Proxy(proxy.clone()),
        Migration {
            cookies: true,
            storage: false,
            url: false,
        },
        "shift_proxy",
    )
    .await?;
    Ok(Value::str(proxy))
}

// ───────────────────────── parallel for ─────────────────────────

fn iterate(v: &Value) -> Result<Vec<Value>, RuntimeError> {
    match v {
        Value::List(l) => Ok(l.borrow().clone()),
        Value::Map(m) => Ok(m.borrow().keys().map(|k| Value::Str(k.clone())).collect()),
        Value::Str(s) => Ok(s.chars().map(|c| Value::str(c.to_string())).collect()),
        Value::Native(n) => n.iter_items().ok_or_else(|| {
            RuntimeError::new(format!(
                "parallel for: cannot iterate over {}",
                n.type_name()
            ))
        }),
        other => Err(RuntimeError::new(format!(
            "parallel for: cannot iterate over {}",
            other.type_name()
        ))),
    }
}

fn short(v: &Value) -> String {
    let s = match v {
        Value::Str(s) => format!("{s:?}"),
        other => other.to_string(),
    };
    if s.chars().count() > 60 {
        let cut: String = s.chars().take(57).collect();
        format!("{cut}…")
    } else {
        s
    }
}

/// Cancels the items still running if the loop is abandoned (the parent
/// task was cancelled while waiting).
struct CancelOnDrop {
    tasks: Vec<Rc<TaskInfo>>,
    done: bool,
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if !self.done {
            for t in &self.tasks {
                if !t.is_done() {
                    t.cancel();
                }
            }
        }
    }
}

/// `parallel for x in items:` with `limit` / `fail_fast` in `opts`: one
/// task per item (each with its own page on first use), at most `limit`
/// running at once; waits for all. Errors are collected into one error
/// listing the failed items — or, with `fail_fast: true`, the first error
/// cancels the rest and is re-raised.
pub async fn parallel_for(
    rt: &Rc<Runtime>,
    vm: &mut Vm,
    items: Value,
    body: Rc<Closure>,
    opts: Args,
) -> Result<Value, RuntimeError> {
    let _ = vm;
    opts.check_kwargs("parallel for", &["limit", "fail_fast"])?;
    let limit = match opts.kw("limit") {
        None | Some(Value::Nil) => None,
        Some(Value::Int(n)) if *n >= 1 => Some(*n as usize),
        Some(v) => {
            return Err(RuntimeError::new(format!(
                "parallel for: limit: must be a positive int, got {v}"
            )))
        }
    };
    let fail_fast = match opts.kw("fail_fast") {
        None | Some(Value::Nil) => false,
        Some(Value::Bool(b)) => *b,
        Some(v) => {
            return Err(RuntimeError::new(format!(
                "parallel for: fail_fast: must be true or false, got {}",
                v.type_name()
            )))
        }
    };
    let items = iterate(&items)?;
    let total = items.len();
    let permits = Rc::new(Semaphore::new(
        limit
            .unwrap_or(Semaphore::MAX_PERMITS)
            .min(Semaphore::MAX_PERMITS),
    ));
    let supervising = current_supervising();
    let tasks: Vec<Rc<TaskInfo>> = items
        .iter()
        .map(|item| {
            spawn_task(
                rt,
                Callee::Item {
                    body: body.clone(),
                    permits: permits.clone(),
                },
                Args::positional(vec![item.clone()]),
                "parallel for",
                Flavor::Item,
                None,
                supervising.clone(),
            )
        })
        .collect();
    let mut guard = CancelOnDrop {
        tasks: tasks.clone(),
        done: false,
    };
    let mut failures: Vec<(usize, RuntimeError)> = Vec::new();
    if fail_fast {
        let mut pending: Vec<(usize, Rc<TaskInfo>)> = tasks.iter().cloned().enumerate().collect();
        while !pending.is_empty() {
            let waits: Vec<LocalBoxFuture<'_, Result<Value, RuntimeError>>> = pending
                .iter()
                .map(|(_, t)| Box::pin(t.wait()) as LocalBoxFuture<'_, _>)
                .collect();
            let (r, idx, _) = futures::future::select_all(waits).await;
            let (i, _) = pending.remove(idx);
            if let Err(e) = r {
                for (_, t) in &pending {
                    t.cancel();
                }
                for (_, t) in &pending {
                    let _ = t.wait().await;
                }
                guard.done = true;
                if !e.is_catchable() {
                    return Err(e);
                }
                let mut e = e;
                e.message = format!(
                    "parallel for: item {} ({}) failed, the rest were cancelled: {}",
                    i + 1,
                    short(&items[i]),
                    e.message
                );
                return Err(e);
            }
        }
    } else {
        for (i, t) in tasks.iter().enumerate() {
            match t.wait().await {
                Ok(_) => {}
                Err(e) if !e.is_catchable() => {
                    for t in &tasks {
                        t.cancel();
                    }
                    guard.done = true;
                    return Err(e);
                }
                Err(e) => failures.push((i, e)),
            }
        }
    }
    guard.done = true;
    if failures.is_empty() {
        return Ok(Value::Nil);
    }
    let mut msg = format!(
        "parallel for: {} of {total} item{} failed",
        failures.len(),
        if total == 1 { "" } else { "s" }
    );
    for (i, e) in &failures {
        let line = e.message.lines().next().unwrap_or("");
        msg.push_str(&format!(
            "\n  item {} ({}): {line}",
            i + 1,
            short(&items[*i])
        ));
    }
    let first = failures.remove(0).1;
    let mut e = RuntimeError::new(msg).with_cause(first);
    e.selector = None;
    Err(e)
}

// ───────────────────────── script objects ─────────────────────────

/// The value `spawn` returns: `h.join()`, `h.cancel()`, `h.send(msg)`,
/// `h.id`, `h.name`, `h.done`. Also the actor ref `send()` accepts.
pub struct TaskHandle {
    rt: Rc<Runtime>,
    /// The task.
    pub info: Rc<TaskInfo>,
}

impl TaskHandle {
    /// Wrap a task.
    pub fn value(rt: &Rc<Runtime>, info: Rc<TaskInfo>) -> Value {
        Value::native(TaskHandle {
            rt: rt.clone(),
            info,
        })
    }
}

impl NativeObject for TaskHandle {
    fn type_name(&self) -> &str {
        match self.info.flavor {
            Flavor::Actor => "actor",
            _ => "task",
        }
    }

    fn call_method<'a>(
        &'a self,
        _vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            match name.as_str() {
                "join" => {
                    args.check_kwargs("join", &[])?;
                    self.info.wait().await
                }
                "cancel" => {
                    args.check_kwargs("cancel", &[])?;
                    self.info.cancel();
                    Ok(Value::Nil)
                }
                "send" => {
                    let msg = args.require(0, "send")?.deep_clone();
                    crate::actors::deliver(&self.rt, &self.info, msg);
                    Ok(Value::Nil)
                }
                other => Err(RuntimeError::new(format!(
                    "{} has no method `{other}`{}",
                    self.type_name(),
                    crate::methods::did_you_mean(other, &["join", "cancel", "send"])
                ))),
            }
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        match name {
            "id" => Some(Value::Int(self.info.id as i64)),
            "name" => Some(Value::str(&self.info.name)),
            "done" => Some(Value::Bool(self.info.is_done())),
            "cancelled" => Some(Value::Bool(self.info.is_cancelled())),
            _ => None,
        }
    }

    fn to_json(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "id": self.info.id,
            "name": self.info.name,
            "done": self.info.is_done(),
        }))
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

/// `self` inside a task / actor body: `self.id`, `self.name`.
pub struct SelfObject {
    info: Rc<TaskInfo>,
}

impl SelfObject {
    /// `self` for `info`.
    pub fn value(info: Rc<TaskInfo>) -> Value {
        Value::native(SelfObject { info })
    }
}

impl NativeObject for SelfObject {
    fn type_name(&self) -> &str {
        "self"
    }

    fn call_method<'a>(
        &'a self,
        _vm: &'a mut Vm,
        name: &str,
        _args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            Err(RuntimeError::new(format!(
                "self has no method `{name}` — its properties are self.id and self.name"
            )))
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        match name {
            "id" => Some(Value::Int(self.info.id as i64)),
            "name" => Some(Value::str(&self.info.name)),
            "pending" => Some(Value::Int(self.info.mailbox.len() as i64)),
            _ => None,
        }
    }

    fn to_json(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "id": self.info.id, "name": self.info.name }))
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}
