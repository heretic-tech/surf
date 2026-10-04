//! The [`Runtime`] — `surf_vm::Host` implementation.

use crate::config::BrowserConfig;
use crate::pages::PageRegistry;
use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use surf_vm::{Args, Closure, Declaration, FsOp, Host, RuntimeError, Value, Vm};

/// Process-level options from the CLI.
#[derive(Debug, Clone, Default)]
pub struct RuntimeOptions {
    /// Log every CDP frame.
    pub trace_cdp: bool,
    /// Override `browser.path`.
    pub chrome_path: Option<std::path::PathBuf>,
    /// Force headless regardless of the script.
    pub force_headless: bool,
}

/// Shared runtime state. Implemented in task 7 (core), 8 (concurrency),
/// 9 (network).
pub struct Runtime {
    opts: RuntimeOptions,
    configs: RefCell<IndexMap<String, BrowserConfig>>,
    pages: PageRegistry,
    functions: RefCell<IndexMap<String, Rc<Closure>>>,
    exit_code: RefCell<Option<i32>>,
}

impl Runtime {
    /// Create an empty runtime.
    pub fn new(opts: RuntimeOptions) -> Rc<Runtime> {
        Rc::new(Runtime {
            opts,
            configs: RefCell::new(IndexMap::new()),
            pages: PageRegistry::default(),
            functions: RefCell::new(IndexMap::new()),
            exit_code: RefCell::new(None),
        })
    }

    /// CLI options.
    pub fn options(&self) -> &RuntimeOptions {
        &self.opts
    }

    /// Page registry.
    pub fn pages(&self) -> &PageRegistry {
        &self.pages
    }

    /// Declared browser configs by alias (`"default"` for the bare block).
    pub fn configs(&self) -> std::cell::Ref<'_, IndexMap<String, BrowserConfig>> {
        self.configs.borrow()
    }

    /// Called after the main body returns: apply the lifetime rule (wait
    /// for handlers / tasks unless `exit` was called), tear down browsers,
    /// and produce the exit code.
    pub async fn finish(&self, main_result: Result<(), RuntimeError>) -> Result<i32, RuntimeError> {
        if let Some(code) = *self.exit_code.borrow() {
            return Ok(code);
        }
        main_result?;
        crate::lifetime::wait_for_quiescence(self).await;
        Ok(0)
    }
}

impl Host for Runtime {
    fn declare(&self, decl: Declaration) {
        match decl {
            Declaration::Config { name, alias, props } if name == "browser" => {
                let key = alias.unwrap_or_else(|| "default".to_string());
                self.configs
                    .borrow_mut()
                    .insert(key, BrowserConfig::from_props(&props));
            }
            Declaration::Fn { name, closure } => {
                self.functions.borrow_mut().insert(name, closure);
            }
            _ => {
                // tasks / actors / supervisors / handlers: task 8
            }
        }
    }

    fn resolve_global(&self, name: &str) -> Option<Value> {
        self.functions
            .borrow()
            .get(name)
            .map(|c| Value::Fn(c.clone()))
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
        Box::pin(async move { crate::tasks::spawn(self, vm, &name, args).await })
    }

    fn parallel_for<'a>(
        &'a self,
        vm: &'a mut Vm,
        items: Value,
        body: Rc<Closure>,
        opts: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        Box::pin(async move { crate::tasks::parallel_for(self, vm, items, body, opts).await })
    }

    fn sleep<'a>(&'a self, d: Duration) -> LocalBoxFuture<'a, ()> {
        Box::pin(tokio::time::sleep(d))
    }

    fn print(&self, s: &str) {
        println!("{s}");
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
