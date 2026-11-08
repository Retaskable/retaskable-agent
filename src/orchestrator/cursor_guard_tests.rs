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
        watch_with(&a, EngineId::Cursor, true).longest,
        Some(Duration::from_secs(5 * 60))
    );
    assert_eq!(
        watch_with(&b, EngineId::Cursor, true).longest,
        Some(Duration::from_secs(90 * 60))
    );
    assert_eq!(
        watch_with(&zero, EngineId::Cursor, true).longest,
        Some(Duration::from_secs(60 * 60))
    );
    for engine in [EngineId::Claude, EngineId::Codex, EngineId::Pi] {
        assert_eq!(watch_with(&a, engine, true).longest, None);
    }
    assert_eq!(
        watch_with(&a, EngineId::Claude, true).quiet,
        quiet_timeout(a.tuning.session_quiet_minutes)
    );
}

#[test]
fn watch_has_no_cursor_limit_with_the_flag_off() {
    let a = ProjectConfig::default();
    assert_eq!(watch_with(&a, EngineId::Cursor, false).longest, None);
}

#[test]
fn watch_for_is_off_until_the_flag_has_an_answer() {
    assert_eq!(watch_for(&ProjectConfig::default(), EngineId::Cursor).longest, None);
}
