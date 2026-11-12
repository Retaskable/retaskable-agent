use std::time::Duration;

use super::*;

#[test]
fn watch_gives_only_cursor_its_own_projects_time_limit() {
    let mut a = ProjectConfig::default();
    a.budget.cursor_max_minutes = 5;
    let mut b = ProjectConfig::default();
    b.budget.cursor_max_minutes = 90;
    let mut zero = ProjectConfig::default();
    zero.budget.cursor_max_minutes = 0;
    assert_eq!(
        watch_for(&a, EngineId::Cursor).longest,
        Some(Duration::from_secs(5 * 60))
    );
    assert_eq!(
        watch_for(&b, EngineId::Cursor).longest,
        Some(Duration::from_secs(90 * 60))
    );
    assert_eq!(
        watch_for(&zero, EngineId::Cursor).longest,
        Some(Duration::from_secs(60 * 60))
    );
    for engine in [EngineId::Claude, EngineId::Codex, EngineId::Pi] {
        assert_eq!(watch_for(&a, engine).longest, None);
    }
    assert_eq!(
        watch_for(&a, EngineId::Claude).quiet,
        quiet_timeout(a.tuning.session_quiet_minutes)
    );
}
