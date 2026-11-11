//! The watch built for a session (engines/04-running-cursor-agent.md).

use super::session::quiet_timeout;
use crate::config::ProjectConfig;
use crate::engines::{EngineId, Watch};

/// The watch for one session, from that session's own project and engine: the watchdog on every engine and the
/// Cursor time limit (`budget.cursorMaxMinutes`, 0 meaning the 60 default) on Cursor only.
pub(crate) fn watch_for(project: &ProjectConfig, engine: EngineId) -> Watch {
    let minutes = match project.budget.cursor_max_minutes {
        0 => 60,
        m => m,
    };
    Watch {
        quiet: quiet_timeout(project.tuning.session_quiet_minutes),
        longest: (engine == EngineId::Cursor).then(|| std::time::Duration::from_secs(u64::from(minutes) * 60)),
    }
}

#[cfg(test)]
#[path = "cursor_guard_tests.rs"]
mod tests;
