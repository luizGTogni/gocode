//! Correlation identifier for one agent run.

use std::fmt;

/// Uniquely identifies one [`crate::AgentRun`], stable for its whole lifetime.
///
/// Lets Gocode correlate events, cancel the correct execution, persist logs, and distinguish
/// consecutive runs in the same session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AgentRunId(uuid::Uuid);

impl AgentRunId {
    /// Creates a fresh, randomly generated run identifier.
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
}

impl Default for AgentRunId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for AgentRunId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::AgentRunId;

    #[test]
    fn consecutive_runs_receive_distinct_identifiers() {
        assert_ne!(AgentRunId::new(), AgentRunId::new());
    }
}
