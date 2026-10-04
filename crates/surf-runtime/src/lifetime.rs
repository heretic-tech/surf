//! Program lifetime (`docs/language.md` § 6.1): a program stays alive while
//! any handler is installed on a page or any task is running; `exit` (from
//! the main body, a handler, or Ctrl-C) ends it at once.
//!
//! Counters live here; the runtime bumps them when it spawns a task or
//! installs an observer and waits on [`Lifetime::quiescent`] after the main
//! body returns. [`Lifetime::exit_signal`] resolves as soon as an exit is
//! requested so the main body can be dropped mid-await (a `goto` waiting
//! on a slow page does not delay Ctrl-C).

use std::cell::{Cell, RefCell};
use surf_vm::CancelToken;
use tokio::sync::Notify;

/// Exit code used when the process is interrupted (Ctrl-C).
pub const EXIT_INTERRUPTED: i32 = 130;

/// Task / handler bookkeeping plus the exit request.
#[derive(Default)]
pub struct Lifetime {
    running: Cell<usize>,
    handlers: Cell<usize>,
    exit: Cell<Option<i32>>,
    failed: Cell<bool>,
    notify: Notify,
    cancel: RefCell<Option<CancelToken>>,
}

impl Lifetime {
    /// Share the cancel token every forked VM checks, so `exit` and Ctrl-C
    /// stop running tasks cooperatively.
    pub fn set_cancel_token(&self, token: CancelToken) {
        *self.cancel.borrow_mut() = Some(token);
    }

    /// A background task (handler invocation, spawned task) started.
    pub fn task_started(&self) {
        self.running.set(self.running.get() + 1);
    }

    /// A background task finished.
    pub fn task_finished(&self) {
        self.running.set(self.running.get().saturating_sub(1));
        self.notify.notify_waiters();
    }

    /// An observer / handler is now installed on a page.
    pub fn handler_installed(&self) {
        self.handlers.set(self.handlers.get() + 1);
    }

    /// An installed observer went away (its page closed).
    pub fn handler_removed(&self) {
        self.handlers.set(self.handlers.get().saturating_sub(1));
        self.notify.notify_waiters();
    }

    /// Running tasks right now.
    pub fn running(&self) -> usize {
        self.running.get()
    }

    /// Installed handlers right now.
    pub fn handlers(&self) -> usize {
        self.handlers.get()
    }

    /// Record that a background task failed with an uncaught error (the
    /// process exit code becomes 1 unless `exit(n)` says otherwise).
    pub fn mark_failed(&self) {
        self.failed.set(true);
    }

    /// Whether any background task failed.
    pub fn failed(&self) -> bool {
        self.failed.get()
    }

    /// `exit(code)`: the first request wins; every VM is cancelled.
    pub fn request_exit(&self, code: i32) {
        if self.exit.get().is_none() {
            self.exit.set(Some(code));
        }
        if let Some(t) = self.cancel.borrow().as_ref() {
            t.cancel();
        }
        self.notify.notify_waiters();
    }

    /// The requested exit code, if any.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit.get()
    }

    /// Resolves when an exit has been requested.
    pub async fn exit_signal(&self) {
        loop {
            let notified = self.notify.notified();
            if self.exit.get().is_some() {
                return;
            }
            notified.await;
        }
    }

    /// Resolves when no task is running and no handler is installed — or
    /// an exit was requested.
    pub async fn quiescent(&self) {
        loop {
            let notified = self.notify.notified();
            if self.exit.get().is_some() || (self.running.get() == 0 && self.handlers.get() == 0) {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn quiescent_waits_for_tasks_and_handlers() {
        let l = std::rc::Rc::new(Lifetime::default());
        l.task_started();
        l.handler_installed();
        let local = tokio::task::LocalSet::new();
        let l2 = l.clone();
        local.spawn_local(async move {
            tokio::task::yield_now().await;
            l2.task_finished();
            tokio::task::yield_now().await;
            l2.handler_removed();
        });
        local
            .run_until(tokio::time::timeout(
                std::time::Duration::from_secs(2),
                l.quiescent(),
            ))
            .await
            .expect("quiescent");
        assert_eq!(l.running(), 0);
        assert_eq!(l.handlers(), 0);
    }

    #[tokio::test]
    async fn exit_unblocks_and_first_code_wins() {
        let l = std::rc::Rc::new(Lifetime::default());
        l.handler_installed();
        let token = CancelToken::new();
        l.set_cancel_token(token.clone());
        let local = tokio::task::LocalSet::new();
        let l2 = l.clone();
        local.spawn_local(async move {
            tokio::task::yield_now().await;
            l2.request_exit(3);
            l2.request_exit(4);
        });
        local
            .run_until(tokio::time::timeout(
                std::time::Duration::from_secs(2),
                async {
                    l.exit_signal().await;
                    l.quiescent().await;
                },
            ))
            .await
            .expect("exit");
        assert_eq!(l.exit_code(), Some(3));
        assert!(token.is_cancelled());
    }
}
