//! The Retaskable agent, in Rust: a port of Retaskable.com's `agent/` (TypeScript), following
//! docs/specs/agent.md there. Split out from `main.rs` so a module can be ported with its tests
//! before the orchestrator loop that will call it exists: a `pub` item here is this crate's own
//! API, not dead code just because `main` doesn't reach it yet.
pub mod api;
pub mod backlog;
pub mod config;
pub mod lanes;
pub mod network;
pub mod password;
pub mod phone;
pub mod session;
pub mod setup_check;
pub mod store;
pub mod tags;
pub mod telemetry;
