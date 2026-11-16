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

#[test]
fn a_pause_lasts_across_restarts_until_resumed_and_start_paused_adds_a_project_only_once() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("paused.json");
    let env = Env::from_pairs([(VAR, "1")]);
    let ids = vec!["a".to_string()];
    let boot = |ids: &[String]| {
        let state = crate::view::PauseState::persisted(&file);
        apply(dir.path(), &env, ids, |p, why| state.set(p, Some(why.to_string())));
        state
    };
    assert!(boot(&ids).reason("a").is_some(), "first seen: paused");
    // Restarted: still paused, though start_paused no longer pauses it.
    assert!(apply(dir.path(), &env, &ids, |_, _| panic!("paused again")).is_empty());
    let state = boot(&ids);
    assert!(state.reason("a").is_some(), "still paused after a restart");
    state.set("a", None);
    let state = boot(&ids);
    assert!(state.reason("a").is_none(), "resumed stays resumed after a restart");
    // A person's own pause lasts too.
    state.set("a", Some("Paused from the site".into()));
    assert_eq!(
        crate::view::PauseState::persisted(&file).reason("a").as_deref(),
        Some("Paused from the site")
    );
}

#[test]
fn a_pause_file_that_is_missing_or_garbage_starts_nothing_paused() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("paused.json");
    assert!(crate::view::PauseState::persisted(&file).reason("a").is_none());
    std::fs::write(&file, "not json").unwrap();
    assert!(crate::view::PauseState::persisted(&file).reason("a").is_none());
}
