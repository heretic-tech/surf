//! Messaging: every spawned task (an `actor` in particular) owns a
//! [`Mailbox`]; `send(ref, msg)` and `broadcast(msg)` deliver deep copies of
//! a value, `receive()` / `wait_for_message()` take the next one (with an
//! optional `timeout:`), `on message:` inside an actor body routes each
//! message to a handler task instead. `self.id` / `self.name` identify the
//! running task.
//!
//! `broadcast` reaches every live task in the sender's supervisor tree,
//! or every live task in the program when the sender is not supervised (or
//! is the main body) — never the sender itself.

use crate::host::Runtime;
use crate::tasks::{current_task, TaskHandle, TaskInfo};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;
use surf_vm::{Args, RuntimeError, Value};
use tokio::sync::Notify;

/// Actor identifier (`self.id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActorId(pub u64);

/// A FIFO of messages with a wake-up.
#[derive(Default)]
pub struct Mailbox {
    queue: RefCell<VecDeque<Value>>,
    notify: Notify,
}

impl Mailbox {
    /// Enqueue a message.
    pub fn push(&self, v: Value) {
        self.queue.borrow_mut().push_back(v);
        self.notify.notify_waiters();
    }

    /// Dequeue without waiting.
    pub fn pop(&self) -> Option<Value> {
        self.queue.borrow_mut().pop_front()
    }

    /// Queued messages.
    pub fn len(&self) -> usize {
        self.queue.borrow().len()
    }

    /// Whether nothing is queued.
    pub fn is_empty(&self) -> bool {
        self.queue.borrow().is_empty()
    }

    /// Dequeue, waiting for a message to arrive.
    pub async fn recv(&self) -> Value {
        loop {
            let notified = self.notify.notified();
            if let Some(v) = self.pop() {
                return v;
            }
            notified.await;
        }
    }

    /// Dequeue, waiting at most `timeout` (`None` on timeout).
    pub async fn recv_timeout(&self, timeout: Duration) -> Option<Value> {
        tokio::time::timeout(timeout, self.recv()).await.ok()
    }
}

fn duration_kw(args: &Args, f: &str) -> Result<Option<Duration>, RuntimeError> {
    match args.kw("timeout") {
        None | Some(Value::Nil) => Ok(None),
        Some(Value::Duration(d)) => Ok(Some(*d)),
        Some(Value::Int(ms)) => Ok(Some(Duration::from_millis((*ms).max(0) as u64))),
        Some(v) => Err(RuntimeError::new(format!(
            "{f}: `timeout:` must be a duration such as 5s, got {}",
            v.type_name()
        ))),
    }
}

/// The task `ref` names: a spawn handle / actor ref or an integer id.
fn target(rt: &Runtime, v: &Value, f: &str) -> Result<Rc<TaskInfo>, RuntimeError> {
    if let Some(h) = v.downcast_native::<TaskHandle>() {
        return Ok(h.info.clone());
    }
    let id = match v {
        Value::Int(id) => *id,
        Value::Map(m) => match m.borrow().get("id") {
            Some(Value::Int(id)) => *id,
            _ => {
                return Err(RuntimeError::new(format!(
                    "{f}: expected an actor ref or id, got a map without `id`"
                )))
            }
        },
        other => {
            return Err(RuntimeError::new(format!(
                "{f}: expected an actor ref (from `spawn`) or an id, got {}",
                other.type_name()
            )))
        }
    };
    rt.task(id as u64)
        .ok_or_else(|| RuntimeError::new(format!("{f}: no live task or actor with id {id}")))
}

/// `send(ref, msg)`.
pub fn send(rt: &Rc<Runtime>, args: Args) -> Result<Value, RuntimeError> {
    args.check_kwargs("send", &[])?;
    let to = target(rt, args.require(0, "send")?, "send")?;
    let msg = args.require(1, "send")?.deep_clone();
    deliver(rt, &to, msg);
    Ok(Value::Nil)
}

/// `broadcast(msg)`: every live task in the sender's supervisor tree (or
/// the whole program), except the sender. Returns the recipient count.
pub fn broadcast(rt: &Rc<Runtime>, args: Args) -> Result<Value, RuntimeError> {
    args.check_kwargs("broadcast", &[])?;
    let msg = args.require(0, "broadcast")?;
    let me = current_task();
    let group = me.as_ref().and_then(|t| t.group.get());
    let mut n = 0;
    for task in rt.live_tasks() {
        if me.as_ref().is_some_and(|m| m.id == task.id) {
            continue;
        }
        if group.is_some() && task.group.get() != group {
            continue;
        }
        deliver(rt, &task, msg.deep_clone());
        n += 1;
    }
    Ok(Value::Int(n))
}

/// Put `msg` in `task`'s mailbox — or hand it straight to the task's
/// `on message:` handlers when it has any.
pub fn deliver(rt: &Rc<Runtime>, task: &Rc<TaskInfo>, msg: Value) {
    let handlers = task.message_handlers.borrow().clone();
    if handlers.is_empty() {
        task.mailbox.push(msg);
        return;
    }
    for h in handlers {
        crate::tasks::dispatch_message(rt, task, h, msg.deep_clone());
    }
}

/// `receive()` / `wait_for_message()` — `receive(timeout: 5s)` is `nil`
/// on timeout.
pub async fn receive(f: &str, args: Args) -> Result<Value, RuntimeError> {
    args.check_kwargs(f, &["timeout"])?;
    let timeout = duration_kw(&args, f)?;
    let Some(task) = current_task() else {
        return Err(RuntimeError::new(format!(
            "{f}() needs a mailbox — call it inside an actor or a spawned task"
        )));
    };
    Ok(match timeout {
        Some(d) => task.mailbox.recv_timeout(d).await.unwrap_or(Value::Nil),
        None => task.mailbox.recv().await,
    })
}
