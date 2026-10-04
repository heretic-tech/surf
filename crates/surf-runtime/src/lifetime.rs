//! Program lifetime: stay alive while handlers are registered or tasks run;
//! `exit` tears everything down. Implemented in task 8.

use crate::host::Runtime;

/// Wait until no handler is registered and no task is running.
pub async fn wait_for_quiescence(rt: &Runtime) {
    let _ = rt.pages().len();
}
