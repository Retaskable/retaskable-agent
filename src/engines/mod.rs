//! Running a session and reading back what it did, the same way whichever AI tool drives it,
//! following docs/specs/engines/02-the-engine-layer.md and `agent/src/engines/` in Retaskable.com
//! (the ground truth this is porting). The runner stays one engine-neutral piece (`runner.rs`);
//! what differs per tool sits behind the `Engine` interface (`engine.rs`), with Claude Code
//! (`claude.rs`), Cursor (`cursor.rs`) and pi (`pi.rs`) all built, and which one a session uses
//! looked up by `choose::engine_for` (docs/specs/engines/03-choosing-the-engine.md) — the
//! orchestrator/pool that actually spawns sessions through this is still a later item.
pub mod choose;
pub mod claude;
pub mod cursor;
pub mod engine;
mod json_result;
pub mod pi;
pub mod runner;
pub mod summarize;
pub mod transcripts;

pub use choose::{Role, engine_for, has_lane_engines, lane_engine};
pub use claude::{resolve_claude, resolve_claude_on_path};
pub use engine::{
    Engine, EngineId, EngineReader, EphemeralSessionConfig, EventKind, SessionEvent, SessionResult, SessionSpec,
    SessionStart, describe_failure, engine_id_from_str, ephemeral_readonly_spec, is_signed_out, signed_out_text,
};
pub use pi::{resolve_pi, resolve_pi_on_path};
pub use runner::{TranscriptSink, engine_of, run_session};
pub use transcripts::{StartedOn, Transcript, read_transcripts, started_on};
