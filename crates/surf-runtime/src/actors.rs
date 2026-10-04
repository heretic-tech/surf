//! Actors: own page + mailbox; `send`, `broadcast`, `receive`,
//! `wait_for_message`, `self.id`. Implemented in task 8.

/// Actor identifier (`self.id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ActorId(pub u64);
