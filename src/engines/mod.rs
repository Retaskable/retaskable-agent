//! Running a session and reading back what it did, the same way whichever AI tool drives it,
//! following docs/specs/engines/02-the-engine-layer.md and `agent/src/engines/` in Retaskable.com
//! (the ground truth this is porting). The runner stays one engine-neutral piece (`runner.rs`);
//! what differs per tool sits behind the `Engine` interface (`engine.rs`), with Claude Code
//! (`claude.rs`) and Cursor (`cursor.rs`) both built — the orchestrator/pool that actually spawns
//! sessions through this is still a later item.
pub mod claude;
pub mod cursor;
pub mod engine;
pub mod runner;
pub mod summarize;
pub mod transcripts;

pub use claude::{resolve_claude, resolve_claude_on_path};
pub use engine::{
    Engine, EngineId, EngineReader, EventKind, SessionEvent, SessionResult, SessionSpec, SessionStart,
    describe_failure, engine_id_from_str, is_signed_out, signed_out_text,
};
pub use runner::{TranscriptSink, engine_of, run_session};
pub use transcripts::{StartedOn, Transcript, read_transcripts, started_on};
