//! Running a session and reading back what it did, the same way whichever AI tool drives it,
//! following docs/specs/engines/02-the-engine-layer.md and `agent/src/engines/` in Retaskable.com
//! (the ground truth this is porting). The runner stays one engine-neutral piece (`runner.rs`);
//! what differs per tool sits behind the `Engine` interface (`engine.rs`), with Claude Code
//! (`claude.rs`) the only engine built so far — a Cursor engine is a later item, dropped in
//! without this module's callers changing.
pub mod claude;
pub mod engine;
pub mod runner;

pub use claude::{resolve_claude, resolve_claude_on_path};
pub use engine::{
    Engine, EngineId, EngineReader, SessionResult, SessionSpec, SessionStart, describe_failure, is_signed_out,
    signed_out_text,
};
pub use runner::{engine_of, run_session};
