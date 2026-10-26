//! The agent's event log and (later) watchdog (docs/specs/agent-telemetry.md).
pub mod events;

pub use events::{EventLog, KEEP_DAYS, MAX_TEXT, RECENT_MS, clean, day_file, prune, read_events, scrub};
