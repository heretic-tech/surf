//! Supervisors: `strategy: one_for_one | one_for_all`, `max_restarts`,
//! `within`. A restart re-launches the child's page (`Page::rebind`) and
//! re-runs its body. Implemented in task 8.

/// Restart strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strategy {
    /// Restart only the failed child.
    #[default]
    OneForOne,
    /// Restart every child when one fails.
    OneForAll,
}

impl Strategy {
    /// Parse the `strategy:` property value.
    pub fn parse(s: &str) -> Option<Strategy> {
        match s {
            "one_for_one" => Some(Strategy::OneForOne),
            "one_for_all" => Some(Strategy::OneForAll),
            _ => None,
        }
    }
}
