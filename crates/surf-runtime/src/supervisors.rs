//! Supervisors: `strategy: one_for_one | one_for_all`, `max_restarts`,
//! `within`.
//!
//! A `supervisor Name:` declaration starts by itself once the program's
//! declarations are registered (its task is spawned from `Host::declare`
//! and first runs when the main body yields). The body runs under the
//! [`SUPERVISING`] task-local, so every `spawn` in it — directly or inside
//! a `parallel for` of the body — registers the new task as a **child**.
//! The supervisor then waits for children to finish: a child that ends
//! normally (or was cancelled) is dropped from the tree; a child that
//! fails is **restarted** — `one_for_one` restarts just that child,
//! `one_for_all` cancels the others and restarts every child. A restart
//! moves the child's private page to a new target in the same browser
//! context ([`surf_browser::RebindTarget::SameContext`], so cookies and
//! storage survive, the tab is at `about:blank`) and re-runs the body with
//! the original arguments. More than `max_restarts` restarts inside
//! `within` fail the supervisor with an error naming the child and its
//! last error; `Name.join()` re-raises it, `Name.stop()` cancels the
//! children.

use crate::host::Runtime;
use crate::tasks::{copy_error, spawn_task, was_cancelled, Callee, Flavor, TaskInfo};
use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use surf_browser::{Migration, RebindTarget};
use surf_vm::{format_duration, Args, Closure, NativeObject, Prop, RuntimeError, Value, Vm};
use tokio::sync::Notify;

/// Restart strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strategy {
    /// Restart only the failed child.
    #[default]
    OneForOne,
    /// Restart every child when one fails.
    OneForAll,
}

impl Strategy {
    /// Parse the `strategy:` property value.
    pub fn parse(s: &str) -> Option<Strategy> {
        match s {
            "one_for_one" => Some(Strategy::OneForOne),
            "one_for_all" => Some(Strategy::OneForAll),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Strategy::OneForOne => "one_for_one",
            Strategy::OneForAll => "one_for_all",
        }
    }
}

/// Default `max_restarts`.
pub const DEFAULT_MAX_RESTARTS: usize = 3;
/// Default `within`.
pub const DEFAULT_WITHIN: Duration = Duration::from_secs(60);

/// A supervised child: how to start it again, and its current task.
struct Child {
    callee: Callee,
    args: Args,
    name: String,
    flavor: Flavor,
    task: RefCell<Rc<TaskInfo>>,
}

/// One running supervisor.
pub struct SupervisorRun {
    /// Unique id (the `group` of its children).
    pub id: u64,
    /// Declared name.
    pub name: String,
    props: IndexMap<Rc<str>, Prop>,
    body: Rc<Closure>,
    strategy: Cell<Strategy>,
    max_restarts: Cell<usize>,
    within: Cell<Duration>,
    children: RefCell<Vec<Rc<Child>>>,
    restarts: RefCell<Vec<Instant>>,
    stopped: Cell<bool>,
    result: RefCell<Option<Result<(), Rc<RuntimeError>>>>,
    notify: Notify,
    joiners: Cell<usize>,
}

tokio::task_local! {
    /// Set while a supervisor body (or a `parallel for` item of it) runs:
    /// `spawn` registers children here.
    pub static SUPERVISING: Rc<SupervisorRun>;
}

/// The supervisor whose body the running code is part of, if any.
pub fn current_supervising() -> Option<Rc<SupervisorRun>> {
    SUPERVISING.try_with(|s| s.clone()).ok()
}

impl SupervisorRun {
    /// Create (not yet started).
    pub fn new(
        id: u64,
        name: &str,
        props: IndexMap<Rc<str>, Prop>,
        body: Rc<Closure>,
    ) -> Rc<SupervisorRun> {
        Rc::new(SupervisorRun {
            id,
            name: name.to_owned(),
            props,
            body,
            strategy: Cell::new(Strategy::OneForOne),
            max_restarts: Cell::new(DEFAULT_MAX_RESTARTS),
            within: Cell::new(DEFAULT_WITHIN),
            children: RefCell::new(Vec::new()),
            restarts: RefCell::new(Vec::new()),
            stopped: Cell::new(false),
            result: RefCell::new(None),
            notify: Notify::new(),
            joiners: Cell::new(0),
        })
    }

    /// Live (not finished) children.
    pub fn live_children(&self) -> usize {
        self.children
            .borrow()
            .iter()
            .filter(|c| !c.task.borrow().is_done())
            .count()
    }

    /// Restarts performed so far.
    pub fn restarts(&self) -> usize {
        self.restarts.borrow().len()
    }

    /// Whether the supervisor has finished.
    pub fn is_done(&self) -> bool {
        self.result.borrow().is_some()
    }

    /// Register a freshly spawned task as a child.
    pub fn adopt(
        &self,
        _rt: &Rc<Runtime>,
        callee: Callee,
        args: Args,
        name: &str,
        flavor: Flavor,
        info: Rc<TaskInfo>,
    ) {
        self.mark(&info);
        self.children.borrow_mut().push(Rc::new(Child {
            callee,
            args,
            name: name.to_owned(),
            flavor,
            task: RefCell::new(info),
        }));
        self.notify.notify_waiters();
    }

    fn mark(&self, info: &TaskInfo) {
        info.supervised.set(true);
        info.group.set(Some(self.id));
        info.keep_page.set(true);
    }

    /// `Name.stop()`: cancel every child; no restarts follow.
    pub fn stop(&self) {
        self.stopped.set(true);
        for c in self.children.borrow().iter() {
            let t = c.task.borrow().clone();
            if !t.is_done() {
                t.cancel();
            }
        }
        self.notify.notify_waiters();
    }

    /// `Name.join()`: wait for the supervisor; its error is re-raised.
    pub async fn wait(&self) -> Result<(), RuntimeError> {
        self.joiners.set(self.joiners.get() + 1);
        let r = loop {
            let notified = self.notify.notified();
            if let Some(r) = self.result.borrow().as_ref() {
                break match r {
                    Ok(()) => Ok(()),
                    Err(e) => Err(copy_error(e)),
                };
            }
            notified.await;
        };
        self.joiners.set(self.joiners.get() - 1);
        r
    }

    /// Start the supervisor on its own task (counts as a running task).
    pub fn start(self: &Rc<Self>, rt: &Rc<Runtime>) {
        rt.lifetime().task_started();
        let rt = rt.clone();
        let sup = self.clone();
        tokio::task::spawn_local(async move {
            let r = tokio::select! {
                r = run(&rt, &sup) => r,
                _ = rt.lifetime().exit_signal() => Err(RuntimeError::cancelled()),
            };
            if let Err(e) = &r {
                if let Some(code) = e.exit_code() {
                    rt.lifetime().request_exit(code);
                } else if !e.is_cancelled() && sup.joiners.get() == 0 {
                    rt.report_failure(&format!("supervisor {}", sup.name), e);
                }
            }
            *sup.result.borrow_mut() = Some(r.map_err(Rc::new));
            sup.notify.notify_waiters();
            rt.lifetime().task_finished();
        });
    }

    async fn resolve_props(&self, vm: &mut Vm) -> Result<(), RuntimeError> {
        let what = |k: &str| format!("supervisor {}: {k}:", self.name);
        for (k, prop) in &self.props {
            match &**k {
                "strategy" => match prop.resolve(vm).await? {
                    Value::Str(s) => match Strategy::parse(&s) {
                        Some(st) => self.strategy.set(st),
                        None => {
                            return Err(RuntimeError::new(format!(
                                "{} expected one_for_one or one_for_all, got {s:?}",
                                what("strategy")
                            )))
                        }
                    },
                    v => {
                        return Err(RuntimeError::new(format!(
                            "{} expected one_for_one or one_for_all, got {}",
                            what("strategy"),
                            v.type_name()
                        )))
                    }
                },
                "max_restarts" => match prop.resolve(vm).await? {
                    Value::Int(n) if n >= 0 => self.max_restarts.set(n as usize),
                    v => {
                        return Err(RuntimeError::new(format!(
                            "{} expected a non-negative int, got {}",
                            what("max_restarts"),
                            v.type_name()
                        )))
                    }
                },
                "within" => match prop.resolve(vm).await? {
                    Value::Duration(d) => self.within.set(d),
                    v => {
                        return Err(RuntimeError::new(format!(
                            "{} expected a duration such as 60s, got {}",
                            what("within"),
                            v.type_name()
                        )))
                    }
                },
                _ => {}
            }
        }
        Ok(())
    }

    /// Start a child's task again, moving its private page (if it has one)
    /// to a new target in the same browser context.
    async fn restart(&self, rt: &Rc<Runtime>, child: &Rc<Child>, why: &str) {
        let old = child.task.borrow().clone();
        let mut page = old
            .ctx
            .page
            .borrow_mut()
            .take()
            .filter(|(_, p)| p.is_open());
        if let Some((b, p)) = &page {
            if let Err(e) = rt
                .rebind(
                    b,
                    p,
                    RebindTarget::SameContext,
                    Migration::default(),
                    "supervisor restart",
                )
                .await
            {
                tracing::debug!(
                    "supervisor {}: could not rebind page {} for restart ({}); using a new page",
                    self.name,
                    p.label(),
                    e.message
                );
                let _ = b.close_page(p).await;
                page = None;
            }
        }
        eprintln!(
            "surf: supervisor {}: restarting {} ({}, restart {}/{}) after: {why}",
            self.name,
            old.label(),
            self.strategy.get().name(),
            self.restarts.borrow().len(),
            self.max_restarts.get()
        );
        let info = spawn_task(
            rt,
            child.callee.clone(),
            child.args.clone(),
            &child.name,
            child.flavor,
            page,
            None,
        );
        self.mark(&info);
        *child.task.borrow_mut() = info;
    }
}

/// The supervisor loop: run the body (children register), then react to
/// children finishing until none is left.
async fn run(rt: &Rc<Runtime>, sup: &Rc<SupervisorRun>) -> Result<(), RuntimeError> {
    let mut vm = rt.task_vm();
    sup.resolve_props(&mut vm).await?;
    SUPERVISING
        .scope(sup.clone(), vm.call(sup.body.clone(), Args::default()))
        .await?;
    loop {
        if sup.stopped.get() {
            break;
        }
        let live: Vec<(Rc<Child>, Rc<TaskInfo>)> = sup
            .children
            .borrow()
            .iter()
            .map(|c| (c.clone(), c.task.borrow().clone()))
            .filter(|(_, t)| !t.is_done())
            .collect();
        if live.is_empty() {
            break;
        }
        let waits: Vec<LocalBoxFuture<'_, Result<Value, RuntimeError>>> = live
            .iter()
            .map(|(_, t)| Box::pin(t.wait()) as LocalBoxFuture<'_, _>)
            .collect();
        let (r, idx, _) = futures::future::select_all(waits).await;
        let (child, task) = live[idx].clone();
        let error = match r {
            Ok(_) => None,
            Err(e) if was_cancelled(&e) => None,
            Err(e) if e.exit_code().is_some() => return Err(e),
            Err(e) => Some(e),
        };
        let Some(e) = error else {
            // Finished (or stopped): drop the child and its page.
            rt.release_private_page(&task.ctx).await;
            sup.children.borrow_mut().retain(|c| !Rc::ptr_eq(c, &child));
            continue;
        };
        if sup.stopped.get() {
            break;
        }
        let now = Instant::now();
        let within = sup.within.get();
        sup.restarts
            .borrow_mut()
            .retain(|t| now.duration_since(*t) < within);
        if sup.restarts.borrow().len() >= sup.max_restarts.get() {
            let failures = sup.restarts.borrow().len() + 1;
            let msg = format!(
                "supervisor {}: {} failed {failures} times within {} (max_restarts: {}) — giving up; last error: {}",
                sup.name,
                task.label(),
                format_duration(&within),
                sup.max_restarts.get(),
                e.message
            );
            sup.stop();
            let children: Vec<Rc<Child>> = sup.children.borrow().clone();
            for c in children {
                let t = c.task.borrow().clone();
                let _ = t.wait().await;
                rt.release_private_page(&t.ctx).await;
            }
            let mut err = RuntimeError::new(msg).with_cause(e);
            err.selector = None;
            return Err(err);
        }
        sup.restarts.borrow_mut().push(now);
        match sup.strategy.get() {
            Strategy::OneForOne => sup.restart(rt, &child, &e.message).await,
            Strategy::OneForAll => {
                let children: Vec<Rc<Child>> = sup.children.borrow().clone();
                for c in &children {
                    let t = c.task.borrow().clone();
                    if !t.is_done() {
                        t.cancel();
                        let _ = t.wait().await;
                    }
                }
                for c in &children {
                    sup.restart(rt, c, &e.message).await;
                }
            }
        }
    }
    Ok(())
}

/// The script value for a declared supervisor: `Crew.join()`,
/// `Crew.stop()`, `Crew.children`, `Crew.restarts`.
pub struct SupervisorObject {
    sup: Rc<SupervisorRun>,
}

impl SupervisorObject {
    /// Wrap a supervisor.
    pub fn value(sup: Rc<SupervisorRun>) -> Value {
        Value::native(SupervisorObject { sup })
    }
}

impl NativeObject for SupervisorObject {
    fn type_name(&self) -> &str {
        "supervisor"
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
                    self.sup.wait().await.map(|_| Value::Nil)
                }
                "stop" => {
                    args.check_kwargs("stop", &[])?;
                    self.sup.stop();
                    Ok(Value::Nil)
                }
                other => Err(RuntimeError::new(format!(
                    "supervisor {} has no method `{other}`{}",
                    self.sup.name,
                    crate::methods::did_you_mean(other, &["join", "stop"])
                ))),
            }
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        match name {
            "name" => Some(Value::str(&self.sup.name)),
            "children" => Some(Value::Int(self.sup.live_children() as i64)),
            "restarts" => Some(Value::Int(self.sup.restarts() as i64)),
            "done" => Some(Value::Bool(self.sup.is_done())),
            _ => None,
        }
    }

    fn to_json(&self) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "supervisor": self.sup.name,
            "children": self.sup.live_children(),
            "restarts": self.sup.restarts(),
        }))
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}
