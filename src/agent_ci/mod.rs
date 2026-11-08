//! docs/specs/agent-ci.md: running CI on this computer. `host.rs` is the fetch step; `report.rs` is the code host
//! side: statuses and the failure comment.
pub mod container;
pub mod docker;
pub mod host;
pub mod priority;
pub mod report;
