//! The agent's event log and (later) watchdog (docs/specs/agent-telemetry.md).
pub mod event;
pub mod events;
pub mod report;
pub mod start_stats;

pub use event::{Body, Entry, Event};
pub use events::{EventLog, KEEP_DAYS, MAX_TEXT, RECENT_MS, clean, day_file, now_ms, prune, read_events, scrub};
pub use report::{find_item, flag, parse_since, read_all, report_text, why_text};
pub use start_stats::{read_start_stats, start_stats_text};
