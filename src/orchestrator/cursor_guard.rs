//! The Cursor runaway guard's switch and the watch built from it (engines/04-running-cursor-agent.md).

use std::sync::Arc;

use super::session::quiet_timeout;
use crate::config::ProjectConfig;
use crate::engines::{EngineId, Watch};

/// The `cursor-time-guard` flag's answer, shared by every session start: Off until [`keep_cursor_guard_fresh`]
/// has had one answer.
static CURSOR_GUARD: std::sync::OnceLock<Arc<crate::flags::Flags>> = std::sync::OnceLock::new();

fn cursor_guard() -> &'static Arc<crate::flags::Flags> {
    CURSOR_GUARD.get_or_init(|| Arc::new(crate::flags::Flags::from_env()))
}

/// Starts asking for the `cursor-time-guard` flag in the background (call once, inside the runtime).
pub fn keep_cursor_guard_fresh() {
    cursor_guard().keep_fresh("cursor-time-guard", std::time::Duration::from_secs(30));
}

/// The watch for one session, from that session's own project and engine: the watchdog on every engine, and,
/// with the `cursor-time-guard` flag On, the Cursor time limit (`budget.cursorMaxMinutes`, 0 meaning the 60
/// default) on Cursor only.
pub(crate) fn watch_for(project: &ProjectConfig, engine: EngineId) -> Watch {
    watch_with(project, engine, cursor_guard().last("cursor-time-guard"))
}

fn watch_with(project: &ProjectConfig, engine: EngineId, guard: bool) -> Watch {
    let minutes = match project.budget.cursor_max_minutes {
        0 => 60,
        m => m,
    };
    Watch {
        quiet: quiet_timeout(project.tuning.session_quiet_minutes),
        longest: (guard && engine == EngineId::Cursor).then(|| std::time::Duration::from_secs(u64::from(minutes) * 60)),
    }
}

#[cfg(test)]
#[path = "cursor_guard_tests.rs"]
mod tests;
