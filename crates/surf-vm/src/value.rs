//! Dynamic values.

use crate::error::RuntimeError;
use crate::vm::Vm;
use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use std::any::Any;
use std::cell::RefCell;
use std::cmp::Ordering;
use std::fmt;
use std::rc::Rc;
use std::time::Duration;

/// A Surf runtime value. `Rc`-based; the VM is single-threaded.
#[derive(Clone)]
pub enum Value {
    /// `nil`
    Nil,
    /// `true` / `false`
    Bool(bool),
    /// 64-bit integer.
    Int(i64),
    /// 64-bit float.
    Float(f64),
    /// Immutable string.
    Str(Rc<str>),
    /// Mutable, shared list.
    List(Rc<RefCell<Vec<Value>>>),
    /// Mutable, shared, insertion-ordered map with string keys.
    Map(Rc<RefCell<IndexMap<Rc<str>, Value>>>),
    /// Duration literal (`500ms`, `2s`, …).
    Duration(Duration),
    /// A script function / lambda / task body.
    Fn(Rc<Closure>),
    /// A host-provided object (page, element, browser, actor ref, …).
    Native(Rc<dyn NativeObject>),
}

impl Value {
    /// Construct a string value.
    pub fn str(s: impl AsRef<str>) -> Value {
        Value::Str(Rc::from(s.as_ref()))
    }

    /// Construct a list value.
    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(RefCell::new(items)))
    }

    /// Construct a map value.
    pub fn map(entries: IndexMap<Rc<str>, Value>) -> Value {
        Value::Map(Rc::new(RefCell::new(entries)))
    }

    /// Wrap a native object.
    pub fn native(n: impl NativeObject + 'static) -> Value {
        Value::Native(Rc::new(n))
    }

    /// Name of the dynamic type, for error messages.
    pub fn type_name(&self) -> &str {
        match self {
            Value::Nil => "nil",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float",
            Value::Str(_) => "str",
            Value::List(_) => "list",
            Value::Map(_) => "map",
            Value::Duration(_) => "duration",
            Value::Fn(_) => "fn",
            Value::Native(n) => n.type_name(),
        }
    }

    /// Truthiness: `nil`, `false`, `0`, `0.0`, `""`, `[]`, `{}` are falsy.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Nil => false,
            Value::Bool(b) => *b,
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::List(l) => !l.borrow().is_empty(),
            Value::Map(m) => !m.borrow().is_empty(),
            Value::Duration(_) | Value::Fn(_) | Value::Native(_) => true,
        }
    }

    /// `true` for `nil`.
    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    /// The string if this is a `Str`.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// The integer if this is an `Int`.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// The number as `f64` if this is an `Int` or `Float`.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Downcast a native object to a concrete type (via
    /// [`NativeObject::as_any`]).
    pub fn downcast_native<T: 'static>(&self) -> Option<&T> {
        match self {
            Value::Native(n) => n.as_any()?.downcast_ref::<T>(),
            _ => None,
        }
    }

    /// Convert to JSON (for `emit`, `eval` arguments, `cookies`, …).
    /// Functions become `null`; natives use [`NativeObject::to_json`].
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Value::Nil => serde_json::Value::Null,
            Value::Bool(b) => serde_json::Value::Bool(*b),
            Value::Int(i) => serde_json::Value::from(*i),
            Value::Float(f) => serde_json::Value::from(*f),
            Value::Str(s) => serde_json::Value::String(s.to_string()),
            Value::List(l) => {
                serde_json::Value::Array(l.borrow().iter().map(Value::to_json).collect())
            }
            Value::Map(m) => serde_json::Value::Object(
                m.borrow()
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_json()))
                    .collect(),
            ),
            Value::Duration(d) => serde_json::Value::from(d.as_millis() as u64),
            Value::Fn(_) => serde_json::Value::Null,
            Value::Native(n) => n.to_json().unwrap_or(serde_json::Value::Null),
        }
    }

    /// Convert from JSON (results of `eval`, `cookies`, …).
    pub fn from_json(v: serde_json::Value) -> Value {
        match v {
            serde_json::Value::Null => Value::Nil,
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    Value::Int(i)
                } else {
                    Value::Float(n.as_f64().unwrap_or(f64::NAN))
                }
            }
            serde_json::Value::String(s) => Value::str(s),
            serde_json::Value::Array(a) => {
                Value::list(a.into_iter().map(Value::from_json).collect())
            }
            serde_json::Value::Object(o) => Value::map(
                o.into_iter()
                    .map(|(k, v)| (Rc::from(k.as_str()), Value::from_json(v)))
                    .collect(),
            ),
        }
    }

    /// Value equality per `docs/language.md` § 2.1: deep for data, identity
    /// for functions and natives, `1 == 1.0`.
    pub fn equals(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => {
                (*a as f64) == *b
            }
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Duration(a), Value::Duration(b)) => a == b,
            (Value::List(a), Value::List(b)) => {
                if Rc::ptr_eq(a, b) {
                    return true;
                }
                let (a, b) = (a.borrow(), b.borrow());
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.equals(y))
            }
            (Value::Map(a), Value::Map(b)) => {
                if Rc::ptr_eq(a, b) {
                    return true;
                }
                let (a, b) = (a.borrow(), b.borrow());
                a.len() == b.len()
                    && a.iter()
                        .all(|(k, v)| b.get(k).map(|w| v.equals(w)).unwrap_or(false))
            }
            (Value::Fn(a), Value::Fn(b)) => Rc::ptr_eq(a, b),
            (Value::Native(a), Value::Native(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Ordering for `<`, `<=`, `>`, `>=`: numbers, strings, durations.
    pub fn compare(&self, other: &Value) -> Option<Ordering> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
            (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
            (Value::Duration(a), Value::Duration(b)) => Some(a.cmp(b)),
            _ => {
                let (a, b) = (self.as_f64()?, other.as_f64()?);
                a.partial_cmp(&b)
            }
        }
    }

    /// Deep copy of lists and maps (used for actor messages); everything
    /// else is shared.
    pub fn deep_clone(&self) -> Value {
        match self {
            Value::List(l) => Value::list(l.borrow().iter().map(Value::deep_clone).collect()),
            Value::Map(m) => Value::map(
                m.borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.deep_clone()))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

/// Human form of a duration: the largest unit that divides it exactly
/// (`1h`, `3m`, `2s`, `500ms`), else fractional seconds (`1.5s`).
pub fn format_duration(d: &Duration) -> String {
    let ms = d.as_millis();
    if ms == 0 {
        return "0ms".into();
    }
    if ms % 3_600_000 == 0 {
        format!("{}h", ms / 3_600_000)
    } else if ms % 60_000 == 0 {
        format!("{}m", ms / 60_000)
    } else if ms % 1000 == 0 {
        format!("{}s", ms / 1000)
    } else if ms > 1000 {
        format!("{}s", ms as f64 / 1000.0)
    } else {
        format!("{ms}ms")
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "nil"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{x:?}"),
            Value::Str(s) => write!(f, "{s:?}"),
            Value::List(l) => f.debug_list().entries(l.borrow().iter()).finish(),
            Value::Map(m) => f.debug_map().entries(m.borrow().iter()).finish(),
            Value::Duration(d) => write!(f, "{}", format_duration(d)),
            Value::Fn(c) => write!(f, "<fn {}>", c.func.name),
            Value::Native(n) => write!(f, "<{}>", n.type_name()),
        }
    }
}

/// `print` / interpolation rules: strings raw, lists and maps as JSON,
/// everything else in its literal form.
impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Str(s) => write!(f, "{s}"),
            Value::List(_) | Value::Map(_) => write!(f, "{}", self.to_json()),
            Value::Native(n) => match n.to_json() {
                Some(j) => write!(f, "{j}"),
                None => write!(f, "<{}>", n.type_name()),
            },
            other => write!(f, "{other:?}"),
        }
    }
}

/// Positional + keyword arguments of a call: `click("#a", timeout: 5s)`.
#[derive(Clone, Debug, Default)]
pub struct Args {
    /// Positional arguments in order.
    pub positional: Vec<Value>,
    /// Keyword arguments in source order.
    pub kwargs: IndexMap<Rc<str>, Value>,
}

impl Args {
    /// Positional-only args.
    pub fn positional(values: Vec<Value>) -> Self {
        Self {
            positional: values,
            kwargs: IndexMap::new(),
        }
    }

    /// Positional argument `i`, if present.
    pub fn get(&self, i: usize) -> Option<&Value> {
        self.positional.get(i)
    }

    /// Keyword argument `name`, if present.
    pub fn kw(&self, name: &str) -> Option<&Value> {
        self.kwargs.get(name)
    }

    /// Positional argument `i`, or an error naming the function.
    pub fn require(&self, i: usize, func: &str) -> Result<&Value, RuntimeError> {
        self.positional.get(i).ok_or_else(|| {
            RuntimeError::new(format!(
                "{func}: missing argument {} (got {})",
                i + 1,
                self.positional.len()
            ))
        })
    }

    /// Error if any keyword argument is not in `allowed` (builtins reject
    /// unknown keywords by name).
    pub fn check_kwargs(&self, func: &str, allowed: &[&str]) -> Result<(), RuntimeError> {
        for k in self.kwargs.keys() {
            if !allowed.contains(&&**k) {
                return Err(RuntimeError::new(format!(
                    "{func}: unknown keyword argument `{k}`"
                )));
            }
        }
        Ok(())
    }
}

/// A captured variable: shared between the closure and the frame that
/// created it so assignments are visible on both sides.
pub type Upvalue = Rc<RefCell<Value>>;

/// A compiled function plus its captured environment.
pub struct Closure {
    /// The compiled function.
    pub func: Rc<crate::bytecode::Function>,
    /// Captured variables (upvalues), indexed by the compiler.
    pub captures: Vec<Upvalue>,
}

impl Closure {
    /// Wrap a function with no captures.
    pub fn new(func: Rc<crate::bytecode::Function>) -> Self {
        Self {
            func,
            captures: Vec::new(),
        }
    }
}

impl fmt::Debug for Closure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<fn {}>", self.func.name)
    }
}

/// A host-provided object exposed to scripts (page, element, browser,
/// actor reference, …). Methods are async because they usually hit CDP.
pub trait NativeObject {
    /// Type name shown in errors and `print`.
    fn type_name(&self) -> &str;

    /// Invoke `obj.name(args)`. Calling the object itself (`obj(args)`)
    /// invokes the method `__call__`.
    fn call_method<'a>(
        &'a self,
        vm: &'a mut Vm,
        name: &str,
        args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>>;

    /// Read `obj.name` (non-call). `None` → "no such property" error.
    fn get_prop(&self, name: &str) -> Option<Value> {
        let _ = name;
        None
    }

    /// JSON form for `emit` and JSON-crossing calls. `None` → `null`.
    fn to_json(&self) -> Option<serde_json::Value> {
        None
    }

    /// Downcasting hook; return `Some(self)` to let the VM and stdlib
    /// recognise the concrete type (ranges, builtin functions).
    fn as_any(&self) -> Option<&dyn Any> {
        None
    }

    /// Snapshot of the items for `for x in obj:` / `len(obj)`. `None` →
    /// not iterable.
    fn iter_items(&self) -> Option<Vec<Value>> {
        None
    }
}

/// `a..b` / `a..=b` — a lazy integer range (iterable; `len` works).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// First value (inclusive).
    pub start: i64,
    /// End (exclusive).
    pub end: i64,
}

impl Range {
    /// Number of items.
    pub fn len(&self) -> usize {
        (self.end - self.start).max(0) as usize
    }

    /// Whether the range is empty.
    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }
}

impl NativeObject for Range {
    fn type_name(&self) -> &str {
        "range"
    }

    fn call_method<'a>(
        &'a self,
        _vm: &'a mut Vm,
        name: &str,
        _args: Args,
    ) -> LocalBoxFuture<'a, Result<Value, RuntimeError>> {
        let name = name.to_string();
        let r = *self;
        Box::pin(async move {
            match name.as_str() {
                "len" => Ok(Value::Int(r.len() as i64)),
                "to_list" => Ok(Value::list((r.start..r.end).map(Value::Int).collect())),
                "contains" => Err(RuntimeError::new("range.contains: not supported")),
                _ => Err(RuntimeError::new(format!(
                    "range has no method `{name}` (available: len, to_list)"
                ))),
            }
        })
    }

    fn get_prop(&self, name: &str) -> Option<Value> {
        match name {
            "start" => Some(Value::Int(self.start)),
            "end" => Some(Value::Int(self.end)),
            _ => None,
        }
    }

    fn to_json(&self) -> Option<serde_json::Value> {
        Some(serde_json::Value::Array(
            (self.start..self.end)
                .map(serde_json::Value::from)
                .collect(),
        ))
    }

    fn as_any(&self) -> Option<&dyn Any> {
        Some(self)
    }

    fn iter_items(&self) -> Option<Vec<Value>> {
        Some((self.start..self.end).map(Value::Int).collect())
    }
}
