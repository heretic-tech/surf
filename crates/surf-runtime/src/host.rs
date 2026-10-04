//! The [`Runtime`] — `surf_vm::Host` implementation and the process-wide
//! state: browser slots, declarations, handlers, the lifetime counters,
//! stdout sinks, and the run / exec / shutdown entry points the CLI uses.

use crate::browsers::{BrowserSlot, DEFAULT_ALIAS};
use crate::config::BrowserConfig;
use crate::handlers::{spawn_observer, HandlerDecl, HandlerEvent};
use crate::lifetime::{Lifetime, EXIT_INTERRUPTED};
use crate::objects::{BareAction, BrowserObject, PageObject};
use crate::supervisors::{SupervisorObject, SupervisorRun};
use crate::tasks::{current_task, SelfObject, TaskDecl, TaskInfo};
use crate::RunError;
use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use std::cell::{Cell, Ref, RefCell};
use std::collections::HashSet;
use std::rc::{Rc, Weak};
use std::time::Duration;
use surf_browser::{Browser, Page};
use surf_vm::{
    Args, Closure, CompiledProgram, Declaration, FsOp, Globals, Host, RuntimeError, Value, Vm,
};

/// Process-level options from the CLI.
#[derive(Debug, Clone, Default)]
pub struct RuntimeOptions {
    /// Log every CDP frame.
    pub trace_cdp: bool,
    /// Override `browser.path` (`--chrome`, `SURF_CHROME`).
    pub chrome_path: Option<std::path::PathBuf>,
    /// `--headless` / `--headed`: force the mode regardless of the script.
    pub headless: Option<bool>,
    /// `--json`: `print` writes `{"print": "…"}` lines so stdout stays
    /// one JSON document per line next to `emit`.
    pub json: bool,
    /// `--timeout`: default action timeout (overrides `timeout:`).
    pub timeout: Option<Duration>,
    /// Render diagnostics with colour.
    pub color: bool,
}

/// The script text, kept for diagnostics and `eval(fn(): …)`.
#[derive(Debug, Clone, Default)]
pub struct Source {
    /// File name shown in diagnostics.
    pub name: String,
    /// Full text.
    pub text: String,
}

/// Shared runtime state (one per `surf run` / REPL session).
pub struct Runtime {
    me: Weak<Runtime>,
    opts: RuntimeOptions,
    source: RefCell<Source>,
    browsers: RefCell<IndexMap<String, Rc<BrowserSlot>>>,
    functions: RefCell<IndexMap<String, Rc<Closure>>>,
    /// `task` / `actor` declarations by name.
    decls: RefCell<IndexMap<String, Rc<TaskDecl>>>,
    /// Supervisors by name (started at declaration).
    supervisors: RefCell<IndexMap<String, Rc<SupervisorRun>>>,
    /// Live tasks by id.
    tasks: RefCell<IndexMap<u64, Rc<TaskInfo>>>,
    next_task_id: Cell<u64>,
    handlers: RefCell<Vec<Rc<HandlerDecl>>>,
    /// Pages (browser alias, page index) that already have an observer.
    observed: RefCell<HashSet<(String, usize)>>,
    /// Tasks' private pages (browser alias, page index).
    pub(crate) private_pages: RefCell<HashSet<(String, usize)>>,
    lifetime: Lifetime,
    template: RefCell<Option<Vm>>,
    globals: Rc<Globals>,
    headless_notice: Cell<bool>,
    warned: RefCell<HashSet<String>>,
}

impl Runtime {
    /// Create an empty runtime.
    pub fn new(opts: RuntimeOptions) -> Rc<Runtime> {
        Rc::new_cyclic(|me| Runtime {
            me: me.clone(),
            opts,
            source: RefCell::new(Source::default()),
            browsers: RefCell::new(IndexMap::new()),
            functions: RefCell::new(IndexMap::new()),
            decls: RefCell::new(IndexMap::new()),
            supervisors: RefCell::new(IndexMap::new()),
            tasks: RefCell::new(IndexMap::new()),
            next_task_id: Cell::new(1),
            handlers: RefCell::new(Vec::new()),
            observed: RefCell::new(HashSet::new()),
            private_pages: RefCell::new(HashSet::new()),
            lifetime: Lifetime::default(),
            template: RefCell::new(None),
            globals: Rc::new(Globals::stdlib()),
            headless_notice: Cell::new(false),
            warned: RefCell::new(HashSet::new()),
        })
    }

    /// A strong handle to this runtime.
    pub fn rc(&self) -> Rc<Runtime> {
        self.me.upgrade().expect("runtime alive")
    }

    /// CLI options.
    pub fn options(&self) -> &RuntimeOptions {
        &self.opts
    }

    /// Lifetime counters / exit request.
    pub fn lifetime(&self) -> &Lifetime {
        &self.lifetime
    }

    /// The current script text.
    pub fn source(&self) -> Ref<'_, Source> {
        self.source.borrow()
    }

    /// Set the script text (the REPL does this per chunk).
    pub fn set_source(&self, name: &str, text: &str) {
        *self.source.borrow_mut() = Source {
            name: name.to_owned(),
            text: text.to_owned(),
        };
    }

    /// A fresh VM sharing this runtime's globals and cancel token.
    pub fn vm(&self) -> Vm {
        match self.template.borrow().as_ref() {
            Some(t) => t.fork(),
            None => {
                let vm = Vm::new(self.rc(), self.globals.clone());
                self.lifetime.set_cancel_token(vm.cancel_token());
                vm
            }
        }
    }

    fn ensure_template(&self) -> Vm {
        if self.template.borrow().is_none() {
            let vm = self.vm();
            *self.template.borrow_mut() = Some(vm.fork());
            return vm;
        }
        self.vm()
    }

    /// A VM for a spawned task: shares the globals but has its **own**
    /// cancel token (`h.cancel()`, supervisor restarts); program exit
    /// reaches tasks through [`Lifetime::exit_signal`] instead.
    pub fn task_vm(&self) -> Vm {
        Vm::new(self.rc(), self.globals.clone())
    }

    // ───────────────────────── tasks ─────────────────────────

    /// Allocate the next task id.
    pub fn next_task_id(&self) -> u64 {
        let id = self.next_task_id.get();
        self.next_task_id.set(id + 1);
        id
    }

    /// Record a live task.
    pub fn register_task(&self, info: Rc<TaskInfo>) {
        self.tasks.borrow_mut().insert(info.id, info);
    }

    /// Forget a finished task.
    pub fn unregister_task(&self, id: u64) {
        self.tasks.borrow_mut().shift_remove(&id);
    }

    /// A live task by id.
    pub fn task(&self, id: u64) -> Option<Rc<TaskInfo>> {
        self.tasks.borrow().get(&id).cloned()
    }

    /// Every live task, in spawn order.
    pub fn live_tasks(&self) -> Vec<Rc<TaskInfo>> {
        self.tasks.borrow().values().cloned().collect()
    }

    /// A declared `task` / `actor` by name.
    pub fn task_decl(&self, name: &str) -> Option<Rc<TaskDecl>> {
        self.decls.borrow().get(name).cloned()
    }

    /// A declared supervisor by name.
    pub fn supervisor(&self, name: &str) -> Option<Rc<SupervisorRun>> {
        self.supervisors.borrow().get(name).cloned()
    }

    /// Names a `spawn` could target (for suggestions).
    pub fn callable_names(&self) -> Vec<String> {
        let mut names = self.user_fn_names();
        names.extend(self.decls.borrow().keys().cloned());
        names
    }

    // ───────────────────────── browsers / declarations ─────────────────────────

    /// Declared slot by alias.
    pub fn slot(&self, alias: &str) -> Option<Rc<BrowserSlot>> {
        self.browsers.borrow().get(alias).cloned()
    }

    /// Declared aliases in declaration order.
    pub fn aliases(&self) -> Vec<String> {
        self.browsers.borrow().keys().cloned().collect()
    }

    /// The slot for `alias`, created with a default config when missing.
    pub fn ensure_slot(&self, alias: &str) -> Rc<BrowserSlot> {
        if let Some(s) = self.slot(alias) {
            return s;
        }
        let slot = BrowserSlot::new(alias, BrowserConfig::default());
        self.browsers
            .borrow_mut()
            .insert(alias.to_owned(), slot.clone());
        slot
    }

    /// Register a `browser:` / `browser <alias>:` block.
    pub fn declare_browser(&self, alias: &str, config: BrowserConfig) {
        match self.slot(alias) {
            Some(slot) if !slot.is_live() => *slot.config.borrow_mut() = config,
            Some(_) => self.warn(&format!(
                "browser {alias}: is already running; the new configuration is ignored"
            )),
            None => {
                let slot = BrowserSlot::new(alias, config);
                self.browsers.borrow_mut().insert(alias.to_owned(), slot);
            }
        }
    }

    /// Every browser that has been launched / attached.
    pub fn live_browsers(&self) -> Vec<Rc<Browser>> {
        self.browsers
            .borrow()
            .values()
            .filter_map(|s| s.browser())
            .collect()
    }

    /// A user `fn` by name.
    pub fn user_fn(&self, name: &str) -> Option<Rc<Closure>> {
        self.functions.borrow().get(name).cloned()
    }

    /// Names of user `fn`s (for suggestions).
    pub fn user_fn_names(&self) -> Vec<String> {
        self.functions.borrow().keys().cloned().collect()
    }

    /// The alias of the slot `browser` belongs to (`default` when unknown).
    pub fn alias_of(&self, browser: &Rc<Browser>) -> String {
        self.browsers
            .borrow()
            .iter()
            .find(|(_, s)| s.browser().is_some_and(|b| Rc::ptr_eq(&b, browser)))
            .map(|(a, _)| a.clone())
            .unwrap_or_else(|| DEFAULT_ALIAS.to_owned())
    }

    /// After `Page::rebind`: the old session (and its observer) is gone, so
    /// install the handlers again on the new one.
    pub fn after_rebind(&self, browser: &Rc<Browser>, page: &Page) {
        let key = (self.alias_of(browser), page.index());
        self.observed.borrow_mut().remove(&key);
        self.instrument(browser);
    }

    /// Declared `element_appears` handlers.
    pub fn element_handlers(&self) -> Vec<Rc<HandlerDecl>> {
        self.handlers_for(HandlerEvent::ElementAppears)
    }

    /// Declared `navigation` handlers.
    pub fn navigation_handlers(&self) -> Vec<Rc<HandlerDecl>> {
        self.handlers_for(HandlerEvent::Navigation)
    }

    fn handlers_for(&self, event: HandlerEvent) -> Vec<Rc<HandlerDecl>> {
        self.handlers
            .borrow()
            .iter()
            .filter(|h| h.event == event)
            .cloned()
            .collect()
    }

    /// Install observers on every page of `browser` that has none yet.
    pub fn instrument(&self, browser: &Rc<Browser>) {
        let alias = self.alias_of(browser);
        for page in browser.pages() {
            let key = (alias.clone(), page.index());
            if self.observed.borrow_mut().insert(key) {
                spawn_observer(self.rc(), browser.clone(), page);
            }
        }
    }

    // ───────────────────────── output ─────────────────────────

    /// One-time notice when the browser defaults to headless.
    pub fn notice_headless(&self) {
        if !self.headless_notice.replace(true) {
            eprintln!("surf: no display found — running headless (set `headless: false` or `virtual: true` to override)");
        }
    }

    /// A one-time warning on stderr.
    pub fn warn(&self, msg: &str) {
        if self.warned.borrow_mut().insert(msg.to_owned()) {
            eprintln!("surf: {msg}");
        }
    }

    /// Render a runtime error as the CLI prints it (ariadne, with the
    /// script line, selector, CDP method; the browser's stderr tail is
    /// part of the message on a crash).
    pub fn render_error(&self, e: &RuntimeError) -> String {
        let src = self.source.borrow();
        e.to_diagnostic()
            .render(&src.name, &src.text, self.opts.color)
    }

    /// Record the outcome of a background task (handler invocation).
    pub fn task_result(&self, what: &str, r: Result<Value, RuntimeError>) {
        match r {
            Ok(_) => {}
            Err(e) if e.is_cancelled() => {}
            Err(e) => match e.exit_code() {
                Some(code) => self.lifetime.request_exit(code),
                None => self.report_failure(what, &e),
            },
        }
    }

    /// Report an uncaught error of a background task / handler /
    /// supervisor on stderr; the exit code becomes 1.
    pub fn report_failure(&self, what: &str, e: &RuntimeError) {
        eprint!("{}", self.render_error(e));
        eprintln!("surf: {what} failed (see above)");
        self.lifetime.mark_failed();
    }

    // ───────────────────────── entry points ─────────────────────────

    /// Parse, compile and run a whole script: the main body, then the
    /// lifetime rule (wait for handlers / tasks), then shutdown. Ctrl-C at
    /// any point exits with 130. Returns the process exit code.
    pub async fn run(self: &Rc<Self>, name: &str, source: &str) -> Result<i32, RunError> {
        let compiled = self.compile(name, source)?;
        let ctrl_c = self.spawn_ctrl_c();
        let main = self.execute(&compiled).await;
        let result = self.finish(main).await;
        self.shutdown().await;
        ctrl_c.abort();
        result.map_err(RunError::Runtime)
    }

    /// Parse, compile and run one chunk on a live runtime (the REPL):
    /// declarations accumulate, browsers and pages stay open, nothing is
    /// waited for or torn down. `exit` inside the chunk is reported as
    /// `Ok(Some(code))`.
    pub async fn exec(self: &Rc<Self>, name: &str, source: &str) -> Result<Option<i32>, RunError> {
        let compiled = self.compile(name, source)?;
        match self.execute(&compiled).await {
            Ok(()) => Ok(self.lifetime.exit_code()),
            Err(e) => match e.exit_code() {
                Some(code) => {
                    self.lifetime.request_exit(code);
                    Ok(Some(code))
                }
                None => Err(RunError::Runtime(e)),
            },
        }
    }

    fn compile(&self, name: &str, source: &str) -> Result<CompiledProgram, RunError> {
        self.set_source(name, source);
        let program = surf_syntax::parse(name, source).map_err(RunError::Syntax)?;
        surf_vm::compile_with_source(name, source, &program)
            .map_err(|e| RunError::Syntax(e.into_diagnostics(name)))
    }

    /// Run the main body, racing it against an exit request.
    async fn execute(self: &Rc<Self>, compiled: &CompiledProgram) -> Result<(), RuntimeError> {
        let mut vm = self.ensure_template();
        tokio::select! {
            r = vm.run(compiled) => r,
            _ = self.lifetime.exit_signal() => {
                Err(RuntimeError::exit(self.lifetime.exit_code().unwrap_or(0)))
            }
        }
    }

    fn spawn_ctrl_c(self: &Rc<Self>) -> tokio::task::JoinHandle<()> {
        let rt = self.clone();
        tokio::task::spawn_local(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                eprintln!("surf: interrupted");
                rt.lifetime.request_exit(EXIT_INTERRUPTED);
            }
        })
    }

    /// After the main body: apply the lifetime rule and compute the exit
    /// code. `exit(n)` wins; an uncaught main error is returned; a failed
    /// handler makes the code 1.
    pub async fn finish(&self, main_result: Result<(), RuntimeError>) -> Result<i32, RuntimeError> {
        match main_result {
            Ok(()) => {}
            Err(e) => match e.exit_code() {
                Some(code) => {
                    self.lifetime.request_exit(code);
                    return Ok(code);
                }
                None => {
                    self.lifetime.request_exit(1);
                    return Err(e);
                }
            },
        }
        self.lifetime.quiescent().await;
        if let Some(code) = self.lifetime.exit_code() {
            return Ok(code);
        }
        self.lifetime
            .request_exit(if self.lifetime.failed() { 1 } else { 0 });
        Ok(self.lifetime.exit_code().unwrap_or(0))
    }

    /// Close every browser Surf launched (shutdown ladder, temp profiles
    /// removed); attached browsers only lose the pages Surf created.
    pub async fn shutdown(&self) {
        let slots: Vec<Rc<BrowserSlot>> = self.browsers.borrow().values().cloned().collect();
        for slot in slots {
            if tokio::time::timeout(Duration::from_secs(15), slot.close())
                .await
                .is_err()
            {
                tracing::warn!("closing browser {} timed out", slot.alias);
            }
        }
        self.observed.borrow_mut().clear();
    }
}

impl Host for Runtime {
    fn declare(&self, decl: Declaration) {
        match decl {
            Declaration::Config { name, alias, props } if name == "browser" => {
                let alias = alias.unwrap_or_else(|| DEFAULT_ALIAS.to_string());
                self.declare_browser(&alias, BrowserConfig::from_props(&props));
            }
            Declaration::Config { name, .. } => {
                self.warn(&format!("unknown config block `{name}:` ignored"));
            }
            Declaration::Fn { name, closure } => {
                self.functions.borrow_mut().insert(name, closure);
            }
            Declaration::Handler { event, args, body } => match HandlerEvent::parse(&event) {
                Some(HandlerEvent::Message) => match current_task() {
                    Some(task) => {
                        task.message_handlers.borrow_mut().push(body.clone());
                        // Messages that arrived before the handler existed.
                        while let Some(msg) = task.mailbox.pop() {
                            crate::tasks::dispatch_message(&self.rc(), &task, body.clone(), msg);
                        }
                    }
                    None => self.warn("on message: only works inside an actor body; ignored"),
                },
                Some(ev @ (HandlerEvent::ElementAppears | HandlerEvent::Navigation)) => {
                    self.handlers.borrow_mut().push(Rc::new(HandlerDecl {
                        event: ev,
                        args,
                        body,
                    }));
                    for b in self.live_browsers() {
                        self.instrument(&b);
                    }
                }
                Some(ev) => {
                    self.warn(&format!(
                        "on {}: is not implemented yet (task 9, network hooks); the handler never fires",
                        ev.name()
                    ));
                    self.handlers.borrow_mut().push(Rc::new(HandlerDecl {
                        event: ev,
                        args,
                        body,
                    }));
                }
                None => self.warn(&format!("unknown handler event `{event}` ignored")),
            },
            Declaration::Task {
                name,
                params,
                props,
                body,
            } => {
                self.decls.borrow_mut().insert(
                    name.clone(),
                    Rc::new(TaskDecl {
                        name,
                        actor: false,
                        params,
                        props,
                        body,
                    }),
                );
            }
            Declaration::Actor {
                name,
                params,
                props,
                body,
            } => {
                self.decls.borrow_mut().insert(
                    name.clone(),
                    Rc::new(TaskDecl {
                        name,
                        actor: true,
                        params,
                        props,
                        body,
                    }),
                );
            }
            Declaration::Supervisor { name, props, body } => {
                if self.supervisors.borrow().contains_key(&name) {
                    self.warn(&format!(
                        "supervisor {name}: already declared and running; the new declaration is ignored"
                    ));
                    return;
                }
                let sup = SupervisorRun::new(self.next_task_id(), &name, props, body);
                sup.start(&self.rc());
                self.supervisors.borrow_mut().insert(name, sup);
            }
        }
    }

    fn resolve_global(&self, name: &str) -> Option<Value> {
        if let Some(f) = self.user_fn(name) {
            return Some(Value::Fn(f));
        }
        match name {
            "page" => return Some(PageObject::sole(self.rc())),
            "browser" => {
                let slot = self.default_slot("goto").ok()?;
                return Some(BrowserObject::value(self.rc(), slot));
            }
            "self" => return current_task().map(SelfObject::value),
            _ => {}
        }
        if let Some(slot) = self.slot(name) {
            return Some(BrowserObject::value(self.rc(), slot));
        }
        if let Some(sup) = self.supervisor(name) {
            return Some(SupervisorObject::value(sup));
        }
        crate::builtins::bare_action_name(name).map(|n| BareAction::value(self.rc(), n))
    }

    fn call_global<'a>(
        &'a self,
        vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move { crate::builtins::call(self, vm, &name, args).await })
    }

    fn spawn<'a>(
        &'a self,
        vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move { crate::tasks::spawn(&self.rc(), vm, &name, args).await })
    }

    fn parallel_for<'a>(
        &'a self,
        vm: &'a mut Vm,
        items: Value,
        body: Rc<Closure>,
        opts: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        Box::pin(async move { crate::tasks::parallel_for(&self.rc(), vm, items, body, opts).await })
    }

    fn sleep<'a>(&'a self, d: Duration) -> LocalBoxFuture<'a, ()> {
        Box::pin(tokio::time::sleep(d))
    }

    fn print(&self, s: &str) {
        if self.opts.json {
            println!("{}", serde_json::json!({ "print": s }));
        } else {
            println!("{s}");
        }
    }

    fn emit(&self, v: &Value) {
        println!("{}", v.to_json());
    }

    fn fs<'a>(&'a self, op: FsOp) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        Box::pin(async move {
            match op {
                FsOp::Read(p) => tokio::fs::read_to_string(&p)
                    .await
                    .map(Value::str)
                    .map_err(|e| RuntimeError::new(format!("read_file({p}): {e}")).with_cause(e)),
                FsOp::Write(p, s) => tokio::fs::write(&p, s)
                    .await
                    .map(|_| Value::Nil)
                    .map_err(|e| RuntimeError::new(format!("write_file({p}): {e}")).with_cause(e)),
                FsOp::Append(p, s) => {
                    use tokio::io::AsyncWriteExt;
                    let mut f = tokio::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&p)
                        .await
                        .map_err(|e| {
                            RuntimeError::new(format!("append_file({p}): {e}")).with_cause(e)
                        })?;
                    f.write_all(s.as_bytes()).await.map_err(|e| {
                        RuntimeError::new(format!("append_file({p}): {e}")).with_cause(e)
                    })?;
                    Ok(Value::Nil)
                }
                FsOp::Exists(p) => Ok(Value::Bool(tokio::fs::metadata(&p).await.is_ok())),
            }
        })
    }

    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}
