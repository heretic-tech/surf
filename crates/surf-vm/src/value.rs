//! Dynamic values.

use crate::error::RuntimeError;
use crate::vm::Vm;
use futures::future::LocalBoxFuture;
use indexmap::IndexMap;
use std::cell::RefCell;
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
            Value::Duration(d) => write!(f, "{}ms", d.as_millis()),
            Value::Fn(c) => write!(f, "<fn {}>", c.func.name),
            Value::Native(n) => write!(f, "<{}>", n.type_name()),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Str(s) => write!(f, "{s}"),
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
}

/// A compiled function plus its captured environment.
pub struct Closure {
    /// The compiled function.
    pub func: Rc<crate::bytecode::Function>,
    /// Captured values (upvalues), indexed by the compiler.
    pub captures: Vec<Value>,
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

/// A host-provided object exposed to scripts (page, element, browser,
/// actor reference, …). Methods are async because they usually hit CDP.
pub trait NativeObject {
    /// Type name shown in errors and `print`.
    fn type_name(&self) -> &str;

    /// Invoke `obj.name(args)`.
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
}
