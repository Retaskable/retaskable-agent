//! The Retaskable agent, in Rust: a port of Retaskable.com's `agent/` (TypeScript), following
//! docs/specs/agent.md there. Split out from `main.rs` so a module can be ported with its tests
//! before the orchestrator loop that will call it exists: a `pub` item here is this crate's own
//! API, not dead code just because `main` doesn't reach it yet.
pub mod api;
pub mod app_menu;
pub mod backlog;
pub mod checks;
pub mod ci_coverage;
pub mod commands;
pub mod config;
pub mod connector;
pub mod decisions;
pub mod engines;
pub mod env_path;
pub mod fs_util;
pub mod galleries;
pub mod git_bin;
pub mod ideas;
pub mod implementer;
pub mod item_lifecycle;
pub mod lane_pool;
pub mod lanes;
pub mod link;
pub mod local_model;
pub mod network;
pub mod orchestrator;
pub mod password;
pub mod paths;
pub mod phone;
pub mod playtest_groups;
pub mod proc;
pub mod profile;
pub mod prompt;
pub mod release;
pub mod release_install;
pub mod release_notes;
pub mod release_update;
pub mod reopen;
pub mod repo;
pub mod review_context;
pub mod reviewer;
pub mod runtime;
pub mod scan;
pub mod scouts;
pub mod settings_server;
pub mod setup_check;
pub mod smoke;
pub mod start_at_login;
pub mod store;
mod sync;
pub mod tags;
pub mod telemetry;
#[cfg(test)]
mod test_support;
pub mod text;
pub mod triage;
pub mod view;
