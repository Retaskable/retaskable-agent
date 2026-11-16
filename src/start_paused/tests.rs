use super::*;

fn run(dir: &Path, env: &Env, projects: &[&str]) -> Vec<String> {
    let projects: Vec<String> = projects.iter().map(|p| p.to_string()).collect();
    apply(dir, env, &projects, |_, _| {})
}

#[test]
fn nothing_is_paused_or_written_unless_the_variable_is_1() {
    let dir = tempfile::tempdir().unwrap();
    for env in [
        Env::empty(),
        Env::from_pairs([(VAR, "0")]),
        Env::from_pairs([(VAR, "true")]),
    ] {
        assert!(run(dir.path(), &env, &["a"]).is_empty());
    }
    assert!(!dir.path().join(FILE).exists());
}

#[test]
fn each_project_starts_paused_once_and_a_later_one_still_does() {
    let dir = tempfile::tempdir().unwrap();
    let env = Env::from_pairs([(VAR, "1")]);
    assert_eq!(run(dir.path(), &env, &["a", "b"]), ["a", "b"]);
    // Resumed since, then restarted: the record keeps it from pausing again.
    assert!(run(dir.path(), &env, &["a", "b"]).is_empty());
    // A project linked afterwards is new, so it starts paused; the others stay as they are.
    assert_eq!(run(dir.path(), &env, &["a", "b", "c"]), ["c"]);
}

#[test]
fn a_computer_with_no_project_yet_records_nothing_so_its_first_still_starts_paused() {
    let dir = tempfile::tempdir().unwrap();
    let env = Env::from_pairs([(VAR, "1")]);
    assert!(run(dir.path(), &env, &[]).is_empty());
    assert!(!dir.path().join(FILE).exists());
    assert_eq!(run(dir.path(), &env, &["a"]), ["a"]);
}

#[test]
fn the_pause_it_sets_is_the_computers_own_and_reads_as_paused() {
    let dir = tempfile::tempdir().unwrap();
    let state = crate::view::PauseState::default();
    let env = Env::from_pairs([(VAR, "1")]);
    apply(dir.path(), &env, &["a".to_string()], |p, why| {
        state.set(p, Some(why.to_string()))
    });
    assert!(state.reason("a").is_some());
    assert!(state.reason("b").is_none());
}
