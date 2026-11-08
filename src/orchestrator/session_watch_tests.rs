//! The timers a session runs under: the watchdog's quiet timeout and a Cursor session's time limit.
use std::time::Duration;

use super::*;

#[test]
fn quiet_timeout_turns_off_at_zero_and_is_minutes_otherwise() {
    assert_eq!(quiet_timeout(0), None);
    assert_eq!(quiet_timeout(20), Some(Duration::from_secs(20 * 60)));
}

#[test]
fn each_projects_cursor_sessions_get_its_own_time_limit_and_other_engines_none() {
    let mut a: ProjectConfig = serde_json::from_str(r#"{"budget":{"cursorMaxMinutes":10}}"#).unwrap();
    let b: ProjectConfig = serde_json::from_str("{}").unwrap();
    let minutes = |p: &ProjectConfig| watch_for(p, EngineId::Cursor).longest;
    assert_eq!(minutes(&a), Some(Duration::from_secs(600)));
    assert_eq!(minutes(&b), Some(Duration::from_secs(3600)));
    a.budget.cursor_max_minutes = 0;
    assert_eq!(minutes(&a), Some(Duration::from_secs(3600)));
    for engine in [EngineId::Claude, EngineId::Codex, EngineId::Pi] {
        assert_eq!(watch_for(&a, engine).longest, None);
    }
    assert_eq!(
        watch_for(&a, EngineId::Claude).quiet,
        quiet_timeout(a.tuning.session_quiet_minutes)
    );
}
