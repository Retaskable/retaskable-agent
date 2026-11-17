//! The pull request label both the code agent and the CI runner read: the one the two programs share
//! through GitHub, so neither imports the other.

/// The label that says a pull request is ready when `ciWhenReady` is on (agent.md section 5).
pub const READY_LABEL: &str = "ci-ready";
