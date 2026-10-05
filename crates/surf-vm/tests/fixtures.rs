//! Fixture harness: every `tests/fixtures/*.surf` is parsed, compiled and
//! run against a recording [`TestHost`]; stdout (prints, emits, host
//! events) must match the sibling `.out` file. Set `UPDATE_FIXTURES=1` to
//! rewrite the expectations.

use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;
use surf_vm::{
    format_duration, Args, Closure, CompileOptions, Declaration, FsOp, Host, NativeObject, Prop,
    RuntimeError, Value, Vm,
};

type TaskDecl = (Rc<Closure>, IndexMap<Rc<str>, Prop>);

#[derive(Default)]
struct TestHost {
    out: RefCell<Vec<String>>,
    fns: RefCell<HashMap<String, Rc<Closure>>>,
    tasks: RefCell<HashMap<String, TaskDecl>>,
    actors: RefCell<HashMap<String, Rc<Closure>>>,
    files: RefCell<HashMap<String, String>>,
    /// Host globals written by `Op::SetGlobal` (REPL mode).
    globals: RefCell<HashMap<String, Value>>,
}

impl TestHost {
    fn line(&self, s: impl Into<String>) {
        self.out.borrow_mut().push(s.into());
    }

    fn prop_names(props: &IndexMap<Rc<str>, Prop>) -> String {
        props
            .iter()
            .map(|(k, p)| match p {
                Prop::Const(v) => format!("{k}={v}"),
                Prop::Lazy(_) => format!("{k}=<lazy>"),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Run a task body honouring `retry` and the lazy `on_fail` prop.
    async fn run_task(&self, vm: &mut Vm, name: &str, args: Args) -> Result<Value, RuntimeError> {
        let (body, props) = self
            .tasks
            .borrow()
            .get(name)
            .cloned()
            .ok_or_else(|| RuntimeError::new(format!("unknown function `{name}`")))?;
        let retry = match props.get("retry") {
            Some(p) => p.resolve(vm).await?.as_int().unwrap_or(0),
            None => 0,
        };
        let mut attempt = 0;
        loop {
            match vm.call(body.clone(), args.clone()).await {
                Ok(v) => return Ok(v),
                Err(e) if e.is_catchable() && attempt < retry => {
                    attempt += 1;
                    self.line(format!("[task {name}] attempt {attempt} failed: {e}"));
                    if let Some(p) = props.get("on_fail") {
                        let v = p.resolve(vm).await?;
                        self.line(format!("[task {name}] on_fail -> {v}"));
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

struct Handle {
    result: Value,
}

impl NativeObject for Handle {
    fn type_name(&self) -> &str {
        "handle"
    }

    fn call_method<'a>(
        &'a self,
        _vm: &'a mut Vm,
        name: &str,
        _args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            match name.as_str() {
                "join" => Ok(self.result.clone()),
                _ => Err(RuntimeError::new(format!("handle has no method `{name}`"))),
            }
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        match name {
            "id" => Some(Value::Int(1)),
            _ => None,
        }
    }
}

impl Host for TestHost {
    fn declare(&self, decl: Declaration) {
        match decl {
            Declaration::Config { name, alias, props } => {
                let alias = alias.map(|a| format!(" {a}")).unwrap_or_default();
                self.line(format!("[declare] {name}{alias}: {}", Value::map(props)));
            }
            Declaration::Fn { name, closure } => {
                self.line(format!(
                    "[declare] fn {name}({})",
                    closure.func.params.join(", ")
                ));
                self.fns.borrow_mut().insert(name, closure);
            }
            Declaration::Task {
                name,
                params,
                props,
                body,
            } => {
                self.line(format!(
                    "[declare] task {name}({}) props: {}",
                    params.join(", "),
                    Self::prop_names(&props)
                ));
                self.tasks.borrow_mut().insert(name, (body, props));
            }
            Declaration::Actor {
                name,
                params,
                props,
                body,
            } => {
                self.line(format!(
                    "[declare] actor {name}({}) props: {}",
                    params.join(", "),
                    Self::prop_names(&props)
                ));
                self.actors.borrow_mut().insert(name, body);
            }
            Declaration::Supervisor { name, props, body } => {
                self.line(format!(
                    "[declare] supervisor {name} props: {} body={}",
                    Self::prop_names(&props),
                    body.func.name
                ));
            }
            Declaration::Handler { event, args, body } => {
                let args: Vec<String> = args.iter().map(|a| format!("{a:?}")).collect();
                self.line(format!(
                    "[declare] on {event}({}) params=[{}]",
                    args.join(", "),
                    body.func.params.join(", ")
                ));
            }
        }
    }

    fn resolve_global(&self, name: &str) -> Option<Value> {
        if let Some(v) = self.globals.borrow().get(name) {
            return Some(v.clone());
        }
        self.fns.borrow().get(name).cloned().map(Value::Fn)
    }

    fn set_global(&self, name: &str, value: Value) -> Result<(), RuntimeError> {
        self.globals.borrow_mut().insert(name.to_string(), value);
        Ok(())
    }

    fn call_global<'a>(
        &'a self,
        vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            if self.tasks.borrow().contains_key(&name) {
                return self.run_task(vm, &name, args).await;
            }
            match name.as_str() {
                // A fake browser action so fixtures can exercise the
                // host-call path and the selector / cdp_method fields.
                "click" => {
                    let sel = args.get(0).map(|v| v.to_string()).unwrap_or_default();
                    args.check_kwargs("click", &["timeout"])?;
                    if sel.starts_with("#missing") {
                        return Err(RuntimeError::new("element not found")
                            .with_selector(sel)
                            .with_cdp_method("DOM.querySelector"));
                    }
                    let timeout = args
                        .kw("timeout")
                        .map(|v| format!(" timeout={v}"))
                        .unwrap_or_default();
                    self.line(format!("[click] {sel}{timeout}"));
                    Ok(Value::Nil)
                }
                _ => Err(RuntimeError::new(format!("unknown function `{name}`"))),
            }
        })
    }

    fn spawn<'a>(
        &'a self,
        vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            self.line(format!("[spawn] {name}"));
            let f = self.fns.borrow().get(&name).cloned();
            let a = self.actors.borrow().get(&name).cloned();
            let result = if let Some(f) = f.or(a) {
                vm.call(f, args).await?
            } else if self.tasks.borrow().contains_key(&name) {
                self.run_task(vm, &name, args).await?
            } else {
                return Err(RuntimeError::new(format!("cannot spawn unknown `{name}`")));
            };
            Ok(Value::native(Handle { result }))
        })
    }

    fn spawn_value<'a>(
        &'a self,
        vm: &'a mut Vm,
        callee: Value,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        Box::pin(async move {
            self.line(format!("[spawn value] {}", callee.type_name()));
            let result = vm.call_value(callee, args).await?;
            Ok(Value::native(Handle { result }))
        })
    }

    fn parallel_for<'a>(
        &'a self,
        vm: &'a mut Vm,
        items: Value,
        body: Rc<Closure>,
        opts: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        Box::pin(async move {
            let opts_s: Vec<String> = opts
                .kwargs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            self.line(format!("[parallel for] opts: {}", opts_s.join(", ")));
            let items: Vec<Value> = match &items {
                Value::List(l) => l.borrow().clone(),
                Value::Native(n) => n
                    .iter_items()
                    .ok_or_else(|| RuntimeError::new("not iterable"))?,
                other => {
                    return Err(RuntimeError::new(format!(
                        "cannot iterate {}",
                        other.type_name()
                    )))
                }
            };
            let fail_fast = opts.kw("fail_fast").map(Value::is_truthy).unwrap_or(false);
            let mut errors = Vec::new();
            for item in items {
                if let Err(e) = vm.call(body.clone(), Args::positional(vec![item])).await {
                    if fail_fast || !e.is_catchable() {
                        return Err(e);
                    }
                    errors.push(e.message);
                }
            }
            if errors.is_empty() {
                Ok(Value::Nil)
            } else {
                Err(RuntimeError::new(format!(
                    "{} item(s) failed: {}",
                    errors.len(),
                    errors.join("; ")
                )))
            }
        })
    }

    fn sleep<'a>(&'a self, d: Duration) -> LocalBoxFuture<'a, ()> {
        self.line(format!("[sleep] {}", format_duration(&d)));
        Box::pin(async {})
    }

    fn print(&self, s: &str) {
        self.line(s);
    }

    fn emit(&self, v: &Value) {
        self.line(serde_json::to_string(&v.to_json()).unwrap());
    }

    fn fs<'a>(&'a self, op: FsOp) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        Box::pin(async move {
            let mut files = self.files.borrow_mut();
            Ok(match op {
                FsOp::Read(p) => match files.get(&p) {
                    Some(s) => Value::str(s),
                    None => return Err(RuntimeError::new(format!("read_file({p}): not found"))),
                },
                FsOp::Write(p, s) => {
                    files.insert(p, s);
                    Value::Nil
                }
                FsOp::Append(p, s) => {
                    files.entry(p).or_default().push_str(&s);
                    Value::Nil
                }
                FsOp::Exists(p) => Value::Bool(files.contains_key(&p)),
            })
        })
    }

    fn env(&self, name: &str) -> Option<String> {
        match name {
            "SURF_TEST_VAR" => Some("hello".into()),
            "SURF_POOL" => Some("wss://pool.example".into()),
            _ => None,
        }
    }

    /// A frozen clock so `now()` is reproducible in fixtures.
    fn now(&self) -> f64 {
        1_700_000_000_001.5
    }

    /// A fixed seed so `random()` is reproducible in fixtures. The
    /// generator state is thread-local and seeded once; every fixture runs
    /// on its own test thread.
    fn random_seed(&self) -> u64 {
        0x5EED_5EED_5EED_5EED
    }
}

fn run_fixture(path: &Path) -> String {
    let source = std::fs::read_to_string(path).unwrap();
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    let program = match surf_syntax::parse(&name, &source) {
        Ok(p) => p,
        Err(d) => return format!("syntax error:\n{}", d.render(&source, false)),
    };
    let compiled = match surf_vm::compile_with_source(&name, &source, &program) {
        Ok(c) => c,
        Err(e) => return format!("compile error: {} at {:?}", e.message, e.span),
    };
    let host = Rc::new(TestHost::default());
    let globals = Rc::new(surf_vm::Globals::stdlib());
    let mut vm = Vm::new(host.clone(), globals);
    let result = futures::executor::block_on(vm.run(&compiled));
    let mut out = host.out.borrow().join("\n");
    match result {
        Ok(()) => {}
        Err(e) => {
            if !out.is_empty() {
                out.push('\n');
            }
            if let Some(code) = e.exit_code() {
                out.push_str(&format!("exit: {code}"));
            } else {
                let line = e
                    .span
                    .map(|s| s.line_col(&source).0)
                    .map(|l| format!(" (line {l})"))
                    .unwrap_or_default();
                out.push_str(&format!("error: {e}{line}"));
            }
        }
    }
    out.push('\n');
    out
}

#[test]
fn fixtures() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "surf").unwrap_or(false))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no fixtures found in {}", dir.display());
    let update = std::env::var("UPDATE_FIXTURES").is_ok();
    let mut failures = Vec::new();
    for path in paths {
        let actual = run_fixture(&path);
        let expected_path = path.with_extension("out");
        if update {
            std::fs::write(&expected_path, &actual).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&expected_path)
            .unwrap_or_else(|_| panic!("missing {}", expected_path.display()));
        if actual != expected {
            failures.push(format!(
                "--- {} ---\nexpected:\n{expected}\nactual:\n{actual}",
                path.display()
            ));
        }
    }
    if !failures.is_empty() {
        panic!(
            "{} fixture(s) failed:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }
}

/// Cold start: parse + compile + run of a 20-line script stays well under
/// 20 ms in a debug build.
#[test]
fn cold_start_under_20ms() {
    let source = r#"
fn fib(n):
    if n < 2:
        return n
    return fib(n - 1) + fib(n - 2)

total = 0
for i in 1..=10:
    total = total + fib(i)
names = ["a", "b", "c"]
m = {}
for n in names:
    m[n] = len(n) + total
s = names.map(fn(x): x.upper()).join("-")
if s == "A-B-C" and m.a == 144:
    print("ok {total}")
else:
    print("bad {s} {m}")
emit {total: total, s: s}
sleep(10ms)
"#;
    let start = std::time::Instant::now();
    let program = surf_syntax::parse("bench.surf", source).unwrap();
    let compiled = surf_vm::compile_with_source("bench.surf", source, &program).unwrap();
    let host = Rc::new(TestHost::default());
    let mut vm = Vm::new(host.clone(), Rc::new(surf_vm::Globals::stdlib()));
    futures::executor::block_on(vm.run(&compiled)).unwrap();
    let elapsed = start.elapsed();
    assert_eq!(
        host.out.borrow().as_slice(),
        [
            "[declare] fn fib(n)",
            "ok 143",
            r#"{"total":143,"s":"A-B-C"}"#,
            "[sleep] 10ms"
        ]
    );
    assert!(
        elapsed < Duration::from_millis(20),
        "cold start took {elapsed:?}"
    );
}

/// `exit` is not catchable and reaches the host as an `ExitRequest`.
#[test]
fn exit_is_not_catchable() {
    let source = "try:\n    exit(3)\ncatch e:\n    print(\"caught\")\nprint(\"after\")\n";
    let program = surf_syntax::parse("x.surf", source).unwrap();
    let compiled = surf_vm::compile("x.surf", &program).unwrap();
    let host = Rc::new(TestHost::default());
    let mut vm = Vm::new(host.clone(), Rc::new(surf_vm::Globals::stdlib()));
    let err = futures::executor::block_on(vm.run(&compiled)).unwrap_err();
    assert_eq!(err.exit_code(), Some(3));
    assert!(host.out.borrow().is_empty());
}

/// Cancellation is observed at loop back-edges.
#[test]
fn cancel_token_stops_loops() {
    let source = "n = 0\nloop:\n    n = n + 1\n    if n == 3:\n        stop()\n";
    let program = surf_syntax::parse("x.surf", source).unwrap();
    let compiled = surf_vm::compile("x.surf", &program).unwrap();
    let host = Rc::new(TestHost::default());
    let globals = Rc::new(surf_vm::Globals::stdlib());
    let mut vm = Vm::new(host.clone(), globals.clone());
    let token = vm.cancel_token();
    globals.register(
        "stop",
        Rc::new(move |_vm, _args| {
            let token = token.clone();
            Box::pin(async move {
                token.cancel();
                Ok(Value::Nil)
            })
        }),
    );
    let err = futures::executor::block_on(vm.run(&compiled)).unwrap_err();
    assert!(err.is_cancelled(), "{err}");
}

/// Compile errors carry a span and convert to diagnostics.
#[test]
fn break_outside_loop_is_a_compile_error() {
    let source = "x = 1\nbreak\n";
    let program = surf_syntax::parse("x.surf", source).unwrap();
    let err = surf_vm::compile("x.surf", &program).unwrap_err();
    assert_eq!(err.message, "`break` outside a loop");
    let rendered = err.into_diagnostics("x.surf").render(source, false);
    assert!(rendered.contains("break"), "{rendered}");
}

/// `CompileOptions::top_level_globals`: top-level assignments go to
/// `Host::set_global` and come back through `Host::resolve_global`, so a
/// REPL can run one line per program and keep its variables.
#[test]
fn top_level_globals_persist_across_programs() {
    let host = Rc::new(TestHost::default());
    let globals = Rc::new(surf_vm::Globals::stdlib());
    let opts = CompileOptions {
        top_level_globals: true,
    };
    let lines = [
        "xs = [1, 2]\n",
        "xs.push(3)\nn = len(xs)\n",
        "n += 1\n",
        "fn bump():\n    return n + 10\n",
        "print(xs, n, bump(), n)\n",
    ];
    for (i, line) in lines.iter().enumerate() {
        let name = format!("repl:{i}");
        let program = surf_syntax::parse(&name, line).unwrap();
        let compiled = surf_vm::compile_with_options(&name, Some(line), &program, opts).unwrap();
        let mut vm = Vm::new(host.clone(), globals.clone());
        futures::executor::block_on(vm.run(&compiled)).unwrap();
    }
    assert_eq!(
        host.out.borrow().as_slice(),
        ["[declare] fn bump()", "[1,2,3] 4 14 4"]
    );
    assert!(host
        .globals
        .borrow()
        .get("n")
        .is_some_and(|v| v.equals(&Value::Int(4))));
}

/// Without the option a top-level assignment is a `<main>` local and the
/// host's global store is never touched.
#[test]
fn top_level_globals_off_by_default() {
    let source = "x = 1\nprint(x)\n";
    let program = surf_syntax::parse("x.surf", source).unwrap();
    let compiled = surf_vm::compile("x.surf", &program).unwrap();
    let host = Rc::new(TestHost::default());
    let mut vm = Vm::new(host.clone(), Rc::new(surf_vm::Globals::stdlib()));
    futures::executor::block_on(vm.run(&compiled)).unwrap();
    assert!(host.globals.borrow().is_empty());
    assert_eq!(host.out.borrow().as_slice(), ["1"]);
}

/// `random()` draws its seed from `Host::random_seed` once per thread: two
/// threads with the same seed produce the same sequence.
#[test]
fn random_is_seeded_by_the_host() {
    fn run() -> Vec<String> {
        let source = "print(random(), random(100), random(5, 9), now())\n";
        let program = surf_syntax::parse("x.surf", source).unwrap();
        let compiled = surf_vm::compile("x.surf", &program).unwrap();
        let host = Rc::new(TestHost::default());
        let mut vm = Vm::new(host.clone(), Rc::new(surf_vm::Globals::stdlib()));
        futures::executor::block_on(vm.run(&compiled)).unwrap();
        let out = host.out.borrow().clone();
        out
    }
    let a = std::thread::spawn(run).join().unwrap();
    let b = std::thread::spawn(run).join().unwrap();
    assert_eq!(a, b);
    assert!(a[0].ends_with(" 1700000000001"), "{a:?}");
}

/// Runtime errors render through `surf_syntax::Diagnostic` with selector
/// and CDP method.
#[test]
fn runtime_error_to_diagnostic() {
    let source = "click(\"#missing\")\n";
    let program = surf_syntax::parse("x.surf", source).unwrap();
    let compiled = surf_vm::compile_with_source("x.surf", source, &program).unwrap();
    let host = Rc::new(TestHost::default());
    let mut vm = Vm::new(host.clone(), Rc::new(surf_vm::Globals::stdlib()));
    let err = futures::executor::block_on(vm.run(&compiled)).unwrap_err();
    assert_eq!(err.selector.as_deref(), Some("#missing"));
    assert_eq!(err.cdp_method.as_deref(), Some("DOM.querySelector"));
    assert!(err.span.is_some());
    let d = err.to_diagnostic();
    let rendered = d.render("x.surf", source, false);
    assert!(rendered.contains("DOM.querySelector"), "{rendered}");
    assert!(rendered.contains("selector: #missing"), "{rendered}");
}
