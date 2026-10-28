//! The Retaskable agent, in Rust: a port of Retaskable.com's `agent/` (TypeScript), following
//! docs/specs/agent.md there. Split out from `main.rs` so a module can be ported with its tests
//! before the orchestrator loop that will call it exists: a `pub` item here is this crate's own
//! API, not dead code just because `main` doesn't reach it yet.
pub mod api;
pub mod backlog;
pub mod ci_coverage;
pub mod config;
pub mod connector;
pub mod engines;
pub mod env_path;
pub mod galleries;
pub mod lanes;
pub mod link;
pub mod local_model;
pub mod network;
pub mod password;
pub mod paths;
pub mod phone;
pub mod release;
pub mod release_install;
pub mod release_update;
pub mod repo;
pub mod scan;
pub mod settings_server;
pub mod setup_check;
pub mod store;
mod sync;
pub mod tags;
pub mod telemetry;
#[cfg(test)]
mod test_support;
