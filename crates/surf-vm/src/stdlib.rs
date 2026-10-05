//! Host-independent builtins (`docs/language.md` § 8).
//!
//! Everything here is pure or routes IO through [`crate::Host`]. Global
//! functions: `len keys values push pop join split trim upper lower contains
//! starts_with ends_with replace json parse_json int float str type range
//! now random read_file write_file append_file env fail sleep print assert`,
//! plus the `json` (`parse`, `stringify`) and `fs` (`read`, `write`,
//! `append`, `exists`) objects. The same names are available as methods on
//! strings, lists and maps (`xs.len()`, `s.upper()`, `m.keys()`).
//!
//! Browser-facing globals (`goto`, `click`, `page`, …) are **not** here —
//! they come from `Host::call_global`.

use crate::error::RuntimeError;
use crate::host::FsOp;
use crate::value::{Args, NativeObject, Range, Value};
use crate::vm::Vm;
use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// A native (Rust) function callable from scripts.
pub type NativeFn =
    Rc<dyn for<'a> Fn(&'a mut Vm, Args) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>>;

/// Registry of global native functions and values shared by every VM in a
/// process.
#[derive(Default)]
pub struct Globals {
    fns: RefCell<IndexMap<Rc<str>, NativeFn>>,
    values: RefCell<IndexMap<Rc<str>, Value>>,
}

impl Globals {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry pre-populated with the standard library.
    pub fn stdlib() -> Self {
        let g = Self::new();
        register_stdlib(&g);
        g
    }

    /// Register / replace a function.
    pub fn register(&self, name: &str, f: NativeFn) {
        self.fns.borrow_mut().insert(Rc::from(name), f);
    }

    /// Register / replace a global value (`json`, `fs`, …).
    pub fn register_value(&self, name: &str, v: Value) {
        self.values.borrow_mut().insert(Rc::from(name), v);
    }

    /// Look up a function.
    pub fn get(&self, name: &str) -> Option<NativeFn> {
        self.fns.borrow().get(name).cloned()
    }

    /// Look up a global value.
    pub fn get_value(&self, name: &str) -> Option<Value> {
        self.values.borrow().get(name).cloned()
    }

    /// All registered names (for `surf check` undefined-name warnings).
    pub fn names(&self) -> Vec<Rc<str>> {
        let mut v: Vec<Rc<str>> = self.fns.borrow().keys().cloned().collect();
        v.extend(self.values.borrow().keys().cloned());
        v
    }
}

/// A stdlib function referenced as a value (`f = len`).
pub struct BuiltinFn {
    /// Function name.
    pub name: Rc<str>,
    /// Implementation.
    pub f: NativeFn,
}

impl NativeObject for BuiltinFn {
    fn type_name(&self) -> &str {
        "fn"
    }

    fn call_method<'a>(
        &'a self,
        vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        if name == "__call__" {
            (self.f)(vm, args)
        } else {
            let msg = format!("fn `{}` has no method `{name}`", self.name);
            Box::pin(async move { Err(RuntimeError::new(msg)) })
        }
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }
}

/// The `json` object: `json.parse(s)`, `json.stringify(v, pretty: false)`.
struct JsonObj;

impl NativeObject for JsonObj {
    fn type_name(&self) -> &str {
        "json"
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
                "parse" => parse_json(args.require(0, "json.parse")?),
                "stringify" => {
                    args.check_kwargs("json.stringify", &["pretty"])?;
                    let v = args.require(0, "json.stringify")?;
                    let pretty = args.kw("pretty").map(Value::is_truthy).unwrap_or(false);
                    to_json_string(v, pretty)
                }
                _ => Err(RuntimeError::new(format!(
                    "json has no method `{name}` (available: parse, stringify)"
                ))),
            }
        })
    }
}

/// The `fs` object: `fs.read(p)`, `fs.write(p, s)`, `fs.append(p, s)`,
/// `fs.exists(p)` — all routed through `Host::fs`.
struct FsObj;

impl NativeObject for FsObj {
    fn type_name(&self) -> &str {
        "fs"
    }

    fn call_method<'a>(
        &'a self,
        vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        Box::pin(async move {
            let op = match name.as_str() {
                "read" => FsOp::Read(str_arg(&args, 0, "fs.read")?),
                "write" => FsOp::Write(
                    str_arg(&args, 0, "fs.write")?,
                    args.require(1, "fs.write")?.to_string(),
                ),
                "append" => FsOp::Append(
                    str_arg(&args, 0, "fs.append")?,
                    args.require(1, "fs.append")?.to_string(),
                ),
                "exists" => FsOp::Exists(str_arg(&args, 0, "fs.exists")?),
                _ => {
                    return Err(RuntimeError::new(format!(
                        "fs has no method `{name}` (available: read, write, append, exists)"
                    )))
                }
            };
            let host = vm.host().clone();
            host.fs(op).await
        })
    }
}

fn str_arg(args: &Args, i: usize, func: &str) -> Result<String, RuntimeError> {
    match args.require(i, func)? {
        Value::Str(s) => Ok(s.to_string()),
        other => Err(RuntimeError::new(format!(
            "{func}: argument {} must be a string, got {}",
            i + 1,
            other.type_name()
        ))),
    }
}

fn parse_json(v: &Value) -> Result<Value, RuntimeError> {
    let Value::Str(s) = v else {
        return Err(RuntimeError::new(format!(
            "parse_json: expected a string, got {}",
            v.type_name()
        )));
    };
    serde_json::from_str::<serde_json::Value>(s)
        .map(Value::from_json)
        .map_err(|e| RuntimeError::new(format!("invalid JSON: {e}")).with_cause(e))
}

fn to_json_string(v: &Value, pretty: bool) -> Result<Value, RuntimeError> {
    let j = v.to_json();
    let s = if pretty {
        serde_json::to_string_pretty(&j)
    } else {
        serde_json::to_string(&j)
    };
    s.map(Value::str)
        .map_err(|e| RuntimeError::new(format!("json: {e}")).with_cause(e))
}

thread_local! {
    static RNG: Cell<u64> = const { Cell::new(0) };
}

/// xorshift64* over a thread-local state, seeded on first use from
/// [`crate::Host::random_seed`].
fn next_random(vm: &Vm) -> u64 {
    RNG.with(|c| {
        let mut x = c.get();
        if x == 0 {
            x = vm.host().random_seed() ^ 0x2545_F491_4F6C_DD1D;
            if x == 0 {
                x = 0x9E37_79B9_7F4A_7C15;
            }
        }
        // xorshift64*
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        c.set(x);
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    })
}

/// Shorthand for registering an async builtin.
fn reg<F>(g: &Globals, name: &'static str, f: F)
where
    F: for<'a> Fn(&'a mut Vm, Args) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> + 'static,
{
    g.register(name, Rc::new(f));
}

/// Register a pure builtin (no VM access).
fn pure<F>(g: &Globals, name: &'static str, f: F)
where
    F: Fn(Args) -> Result<Value, RuntimeError> + 'static,
{
    let f = Rc::new(f);
    g.register(
        name,
        Rc::new(move |_vm, args| {
            let f = f.clone();
            Box::pin(async move { f(args) })
        }),
    );
}

/// Register a global that forwards to the method of the same name on its
/// first argument (`len(xs)` ≡ `xs.len()`).
fn forward(g: &Globals, name: &'static str) {
    reg(g, name, move |vm, mut args| {
        Box::pin(async move {
            if args.positional.is_empty() {
                return Err(RuntimeError::new(format!("{name}: missing argument")));
            }
            let recv = args.positional.remove(0);
            call_method(vm, recv, name, args).await
        })
    });
}

fn register_stdlib(g: &Globals) {
    for name in [
        "len",
        "keys",
        "values",
        "push",
        "pop",
        "join",
        "split",
        "trim",
        "upper",
        "lower",
        "contains",
        "starts_with",
        "ends_with",
        "replace",
    ] {
        forward(g, name);
    }

    pure(g, "json", |args| {
        args.check_kwargs("json", &["pretty"])?;
        let pretty = args.kw("pretty").map(Value::is_truthy).unwrap_or(false);
        to_json_string(args.require(0, "json")?, pretty)
    });
    pure(g, "parse_json", |args| {
        parse_json(args.require(0, "parse_json")?)
    });
    pure(g, "str", |args| {
        Ok(Value::str(args.require(0, "str")?.to_string()))
    });
    pure(g, "int", |args| to_int(args.require(0, "int")?));
    pure(g, "float", |args| to_float(args.require(0, "float")?));
    pure(g, "type", |args| {
        Ok(Value::str(args.require(0, "type")?.type_name()))
    });
    pure(g, "range", |args| {
        let (start, end) = match (args.get(0), args.get(1)) {
            (Some(Value::Int(n)), None) => (0, *n),
            (Some(Value::Int(a)), Some(Value::Int(b))) => (*a, *b),
            _ => return Err(RuntimeError::new("range: expected one or two ints")),
        };
        Ok(Value::native(Range { start, end }))
    });
    reg(g, "now", |vm, _args| {
        Box::pin(async move { Ok(Value::Int(vm.host().now() as i64)) })
    });
    reg(g, "random", |vm, args| {
        Box::pin(async move {
            match (args.get(0), args.get(1)) {
                (None, _) => Ok(Value::Float(
                    (next_random(vm) >> 11) as f64 / (1u64 << 53) as f64,
                )),
                (Some(Value::Int(n)), None) => {
                    if *n <= 0 {
                        return Err(RuntimeError::new("random(n): n must be positive"));
                    }
                    Ok(Value::Int((next_random(vm) % *n as u64) as i64))
                }
                (Some(Value::Int(a)), Some(Value::Int(b))) => {
                    if b <= a {
                        return Err(RuntimeError::new("random(a, b): b must be greater than a"));
                    }
                    Ok(Value::Int(a + (next_random(vm) % (b - a) as u64) as i64))
                }
                _ => Err(RuntimeError::new("random: expected zero, one or two ints")),
            }
        })
    });
    pure(g, "fail", |args| {
        args.check_kwargs("fail", &["selector", "cdp_method"])?;
        let msg = args
            .get(0)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "failed".to_string());
        let mut e = RuntimeError::new(msg);
        if let Some(Value::Str(s)) = args.kw("selector") {
            e = e.with_selector(s.to_string());
        }
        if let Some(Value::Str(m)) = args.kw("cdp_method") {
            e = e.with_cdp_method(m.to_string());
        }
        Err(e)
    });
    pure(g, "assert", |args| {
        let cond = args.require(0, "assert")?;
        if cond.is_truthy() {
            Ok(Value::Nil)
        } else {
            let msg = args
                .get(1)
                .map(|v| format!("assertion failed: {v}"))
                .unwrap_or_else(|| "assertion failed".to_string());
            Err(RuntimeError::new(msg))
        }
    });

    reg(g, "print", |vm, args| {
        Box::pin(async move {
            let line = args
                .positional
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            vm.host().print(&line);
            Ok(Value::Nil)
        })
    });
    reg(g, "env", |vm, args| {
        Box::pin(async move {
            let name = str_arg(&args, 0, "env")?;
            Ok(match vm.host().env(&name) {
                Some(v) => Value::str(v),
                None => args.get(1).cloned().unwrap_or(Value::Nil),
            })
        })
    });
    reg(g, "sleep", |vm, args| {
        Box::pin(async move {
            let d = match args.require(0, "sleep")? {
                Value::Duration(d) => *d,
                Value::Int(ms) => Duration::from_millis((*ms).max(0) as u64),
                Value::Float(s) => Duration::from_secs_f64(s.max(0.0)),
                other => {
                    return Err(RuntimeError::new(format!(
                        "sleep: expected a duration, got {}",
                        other.type_name()
                    )))
                }
            };
            let host = vm.host().clone();
            host.sleep(d).await;
            Ok(Value::Nil)
        })
    });
    reg(g, "read_file", |vm, args| {
        Box::pin(async move {
            let p = str_arg(&args, 0, "read_file")?;
            let host = vm.host().clone();
            host.fs(FsOp::Read(p)).await
        })
    });
    reg(g, "write_file", |vm, args| {
        Box::pin(async move {
            let p = str_arg(&args, 0, "write_file")?;
            let s = args.require(1, "write_file")?.to_string();
            let host = vm.host().clone();
            host.fs(FsOp::Write(p, s)).await
        })
    });
    reg(g, "append_file", |vm, args| {
        Box::pin(async move {
            let p = str_arg(&args, 0, "append_file")?;
            let s = args.require(1, "append_file")?.to_string();
            let host = vm.host().clone();
            host.fs(FsOp::Append(p, s)).await
        })
    });

    g.register_value("json", Value::native(JsonObj));
    g.register_value("fs", Value::native(FsObj));
}

fn to_int(v: &Value) -> Result<Value, RuntimeError> {
    match v {
        Value::Int(i) => Ok(Value::Int(*i)),
        Value::Float(f) => Ok(Value::Int(*f as i64)),
        Value::Bool(b) => Ok(Value::Int(*b as i64)),
        Value::Str(s) => s
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| RuntimeError::new(format!("int: cannot parse {s:?}"))),
        Value::Duration(d) => Ok(Value::Int(d.as_millis() as i64)),
        other => Err(RuntimeError::new(format!(
            "int: cannot convert {}",
            other.type_name()
        ))),
    }
}

fn to_float(v: &Value) -> Result<Value, RuntimeError> {
    match v {
        Value::Int(i) => Ok(Value::Float(*i as f64)),
        Value::Float(f) => Ok(Value::Float(*f)),
        Value::Str(s) => s
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| RuntimeError::new(format!("float: cannot parse {s:?}"))),
        Value::Duration(d) => Ok(Value::Float(d.as_secs_f64())),
        other => Err(RuntimeError::new(format!(
            "float: cannot convert {}",
            other.type_name()
        ))),
    }
}

/// Method call on a non-native receiver (`str`, `list`, `map`, `duration`,
/// …). Maps fall back to calling a function stored under `name`.
pub fn call_method<'a>(
    vm: &'a mut Vm,
    recv: Value,
    name: &str,
    args: Args,
) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
    let name = name.to_string();
    Box::pin(async move {
        match &recv {
            Value::Str(s) => str_method(s, &name, &args),
            Value::List(l) => list_method(vm, l, &name, args).await,
            Value::Map(m) => {
                let r = map_method(m, &name, &args);
                match r {
                    Some(r) => r,
                    None => {
                        let f = m.borrow().get(name.as_str()).cloned();
                        match f {
                            Some(f @ Value::Fn(_)) => vm.call_value(f, args).await,
                            _ => Err(RuntimeError::new(format!(
                                "map has no method `{name}` (available: keys, values, get, has, remove, len)"
                            ))),
                        }
                    }
                }
            }
            Value::Duration(d) => match name.as_str() {
                "ms" => Ok(Value::Int(d.as_millis() as i64)),
                "seconds" => Ok(Value::Float(d.as_secs_f64())),
                _ => Err(RuntimeError::new(format!(
                    "duration has no method `{name}` (available: ms, seconds)"
                ))),
            },
            Value::Native(n) => n.call_method(vm, &name, args).await,
            other => Err(RuntimeError::new(format!(
                "{} has no method `{name}`",
                other.type_name()
            ))),
        }
    })
}

fn expect_str<'v>(v: &'v Value, func: &str) -> Result<&'v str, RuntimeError> {
    match v {
        Value::Str(s) => Ok(s),
        other => Err(RuntimeError::new(format!(
            "{func}: expected a string, got {}",
            other.type_name()
        ))),
    }
}

fn str_method(s: &str, name: &str, args: &Args) -> Result<Value, RuntimeError> {
    let func = format!("str.{name}");
    Ok(match name {
        "len" => Value::Int(s.chars().count() as i64),
        "upper" => Value::str(s.to_uppercase()),
        "lower" => Value::str(s.to_lowercase()),
        "trim" => Value::str(s.trim()),
        "split" => {
            let parts: Vec<Value> = match args.get(0) {
                None => s.split_whitespace().map(Value::str).collect(),
                Some(sep) => {
                    let sep = expect_str(sep, &func)?;
                    if sep.is_empty() {
                        s.chars().map(|c| Value::str(c.to_string())).collect()
                    } else {
                        s.split(sep).map(Value::str).collect()
                    }
                }
            };
            Value::list(parts)
        }
        "lines" => Value::list(s.lines().map(Value::str).collect()),
        "chars" => Value::list(s.chars().map(|c| Value::str(c.to_string())).collect()),
        "contains" => Value::Bool(s.contains(expect_str(args.require(0, &func)?, &func)?)),
        "starts_with" => Value::Bool(s.starts_with(expect_str(args.require(0, &func)?, &func)?)),
        "ends_with" => Value::Bool(s.ends_with(expect_str(args.require(0, &func)?, &func)?)),
        "replace" => {
            let from = expect_str(args.require(0, &func)?, &func)?;
            let to = expect_str(args.require(1, &func)?, &func)?;
            Value::str(s.replace(from, to))
        }
        "to_int" => to_int(&Value::str(s))?,
        "to_float" => to_float(&Value::str(s))?,
        "index_of" => {
            let needle = expect_str(args.require(0, &func)?, &func)?;
            match s.find(needle) {
                Some(b) => Value::Int(s[..b].chars().count() as i64),
                None => Value::Int(-1),
            }
        }
        "slice" => {
            let chars: Vec<char> = s.chars().collect();
            let n = chars.len() as i64;
            let norm = |i: i64| -> usize { (if i < 0 { n + i } else { i }).clamp(0, n) as usize };
            let start = norm(args.get(0).and_then(Value::as_int).unwrap_or(0));
            let end = norm(args.get(1).and_then(Value::as_int).unwrap_or(n));
            Value::str(chars[start..end.max(start)].iter().collect::<String>())
        }
        _ => {
            return Err(RuntimeError::new(format!(
                "str has no method `{name}` (available: len, upper, lower, trim, split, lines, chars, contains, starts_with, ends_with, replace, to_int, to_float, index_of, slice)"
            )))
        }
    })
}

async fn list_method(
    vm: &mut Vm,
    l: &Rc<RefCell<Vec<Value>>>,
    name: &str,
    args: Args,
) -> Result<Value, RuntimeError> {
    let func = format!("list.{name}");
    Ok(match name {
        "len" => Value::Int(l.borrow().len() as i64),
        "push" => {
            for v in args.positional {
                l.borrow_mut().push(v);
            }
            Value::Nil
        }
        "pop" => l.borrow_mut().pop().unwrap_or(Value::Nil),
        "first" => l.borrow().first().cloned().unwrap_or(Value::Nil),
        "last" => l.borrow().last().cloned().unwrap_or(Value::Nil),
        "contains" => {
            let needle = args.require(0, &func)?;
            Value::Bool(l.borrow().iter().any(|v| v.equals(needle)))
        }
        "index_of" => {
            let needle = args.require(0, &func)?;
            Value::Int(
                l.borrow()
                    .iter()
                    .position(|v| v.equals(needle))
                    .map(|i| i as i64)
                    .unwrap_or(-1),
            )
        }
        "join" => {
            let sep = match args.get(0) {
                Some(v) => expect_str(v, &func)?.to_string(),
                None => String::new(),
            };
            let parts: Vec<String> = l.borrow().iter().map(|v| v.to_string()).collect();
            Value::str(parts.join(&sep))
        }
        "reverse" => {
            let mut v = l.borrow().clone();
            v.reverse();
            Value::list(v)
        }
        "slice" => {
            let items = l.borrow().clone();
            let n = items.len() as i64;
            let norm = |i: i64| -> usize { (if i < 0 { n + i } else { i }).clamp(0, n) as usize };
            let start = norm(args.get(0).and_then(Value::as_int).unwrap_or(0));
            let end = norm(args.get(1).and_then(Value::as_int).unwrap_or(n));
            Value::list(items[start..end.max(start)].to_vec())
        }
        "map" => {
            let f = args.require(0, &func)?.clone();
            let items = l.borrow().clone();
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(vm.call_value(f.clone(), Args::positional(vec![item])).await?);
            }
            Value::list(out)
        }
        "filter" => {
            let f = args.require(0, &func)?.clone();
            let items = l.borrow().clone();
            let mut out = Vec::new();
            for item in items {
                if vm
                    .call_value(f.clone(), Args::positional(vec![item.clone()]))
                    .await?
                    .is_truthy()
                {
                    out.push(item);
                }
            }
            Value::list(out)
        }
        "each" => {
            let f = args.require(0, &func)?.clone();
            let items = l.borrow().clone();
            for item in items {
                vm.call_value(f.clone(), Args::positional(vec![item])).await?;
            }
            Value::Nil
        }
        "sort" => {
            args.check_kwargs(&func, &["by", "reverse"])?;
            let items = l.borrow().clone();
            let key_fn = args.get(0).cloned().or_else(|| args.kw("by").cloned());
            let mut keys = Vec::with_capacity(items.len());
            for item in &items {
                keys.push(match &key_fn {
                    Some(f) => {
                        vm.call_value(f.clone(), Args::positional(vec![item.clone()]))
                            .await?
                    }
                    None => item.clone(),
                });
            }
            let mut idx: Vec<usize> = (0..items.len()).collect();
            let mut err = None;
            idx.sort_by(|&a, &b| match keys[a].compare(&keys[b]) {
                Some(o) => o,
                None => {
                    if err.is_none() {
                        err = Some(RuntimeError::new(format!(
                            "sort: cannot compare {} with {}",
                            keys[a].type_name(),
                            keys[b].type_name()
                        )));
                    }
                    std::cmp::Ordering::Equal
                }
            });
            if let Some(e) = err {
                return Err(e);
            }
            if args.kw("reverse").map(Value::is_truthy).unwrap_or(false) {
                idx.reverse();
            }
            Value::list(idx.into_iter().map(|i| items[i].clone()).collect())
        }
        _ => {
            return Err(RuntimeError::new(format!(
                "list has no method `{name}` (available: len, push, pop, first, last, contains, index_of, join, reverse, slice, map, filter, each, sort)"
            )))
        }
    })
}

fn map_method(
    m: &Rc<RefCell<IndexMap<Rc<str>, Value>>>,
    name: &str,
    args: &Args,
) -> Option<Result<Value, RuntimeError>> {
    let func = format!("map.{name}");
    let key = |i: usize| -> Result<Rc<str>, RuntimeError> {
        match args.require(i, &func)? {
            Value::Str(s) => Ok(s.clone()),
            other => Err(RuntimeError::new(format!(
                "{func}: key must be a string, got {}",
                other.type_name()
            ))),
        }
    };
    Some(Ok(match name {
        "len" => Value::Int(m.borrow().len() as i64),
        "keys" => Value::list(m.borrow().keys().map(|k| Value::Str(k.clone())).collect()),
        "values" => Value::list(m.borrow().values().cloned().collect()),
        "has" => match key(0) {
            Ok(k) => Value::Bool(m.borrow().contains_key(&k)),
            Err(e) => return Some(Err(e)),
        },
        "get" => match key(0) {
            Ok(k) => m
                .borrow()
                .get(&k)
                .cloned()
                .unwrap_or_else(|| args.get(1).cloned().unwrap_or(Value::Nil)),
            Err(e) => return Some(Err(e)),
        },
        "remove" => match key(0) {
            Ok(k) => m.borrow_mut().shift_remove(&k).unwrap_or(Value::Nil),
            Err(e) => return Some(Err(e)),
        },
        "set" => match key(0) {
            Ok(k) => {
                let v = match args.require(1, &func) {
                    Ok(v) => v.clone(),
                    Err(e) => return Some(Err(e)),
                };
                m.borrow_mut().insert(k, v);
                Value::Nil
            }
            Err(e) => return Some(Err(e)),
        },
        "entries" => Value::list(
            m.borrow()
                .iter()
                .map(|(k, v)| Value::list(vec![Value::Str(k.clone()), v.clone()]))
                .collect(),
        ),
        _ => return None,
    }))
}
