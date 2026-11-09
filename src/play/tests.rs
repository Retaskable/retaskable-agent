use std::path::PathBuf;
use std::time::{Duration, Instant};

use super::*;
use crate::config::GalleryConfig;
use crate::test_support::repo_with_origin;

#[cfg(windows)]
const LONG_RUNNING: &str = "echo %RETASKABLE_LANE%> started.txt && ping -n 60 127.0.0.1 >nul";
#[cfg(not(windows))]
const LONG_RUNNING: &str = "echo $RETASKABLE_LANE > started.txt && sleep 60";

fn project(id: &str, root: &std::path::Path, command: &str) -> ProjectView {
    ProjectView {
        id: id.into(),
        github_repo: "o/one".into(),
        backlog_path: "backlog".into(),
        repo_path: repo_with_origin(root),
        worktrees_dir: root.join("worktrees"),
        base_branch: "main".into(),
        galleries: vec![],
        play_command: command.into(),
    }
}

fn wait_for(what: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !what() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(what());
}

#[test]
fn a_blank_play_command_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let p = project("play-blank", root.path(), "  ");
    assert_eq!(
        start("play-blank", &p).unwrap_err(),
        "No play command set for this project."
    );
}

#[test]
fn stopping_when_nothing_runs_says_so() {
    assert_eq!(stop("play-idle").unwrap_err(), "Play main isn't running.");
}

#[test]
fn play_holds_the_base_checkout_until_stop_ends_its_process_tree() {
    let root = tempfile::tempdir().unwrap();
    let id = "play-lifecycle";
    let p = project(id, root.path(), LONG_RUNNING);
    start(id, &p).unwrap();
    assert!(running(id));

    let marker: PathBuf = lanes::base_checkout_path(&p.worktrees_dir).join("started.txt");
    wait_for(|| marker.exists() && std::fs::read_to_string(&marker).is_ok_and(|s| s.trim() == "play"));

    assert_eq!(start(id, &p).unwrap_err(), "Play main is already running.");
    let gallery = GalleryConfig {
        name: "Intro".into(),
        command: "true".into(),
        out_dir: "shots".into(),
    };
    assert_eq!(
        crate::galleries::shoot_refusal(id, &gallery).unwrap(),
        "The base checkout is busy: Play main is using it."
    );

    stop(id).unwrap();
    wait_for(|| !running(id) && crate::base_hold::base_holder(id).is_none());
    assert!(crate::galleries::shoot_refusal(id, &gallery).is_none());
}

#[test]
fn play_is_refused_while_a_shoot_holds_the_base_checkout() {
    let root = tempfile::tempdir().unwrap();
    let id = "play-vs-shoot";
    let p = project(id, root.path(), LONG_RUNNING);
    let _shoot = crate::base_hold::hold_base(id, "a shoot").unwrap();
    assert_eq!(
        start(id, &p).unwrap_err(),
        "The base checkout is busy: a shoot is using it."
    );
    assert!(!running(id));
}

#[test]
fn restart_starts_when_nothing_runs_and_replaces_a_running_process() {
    let root = tempfile::tempdir().unwrap();
    let id = "play-restart";
    let p = project(id, root.path(), LONG_RUNNING);
    restart(id, &p).unwrap();
    assert!(running(id));
    let first = lock(&RUNNING).get(id).cloned().unwrap();
    restart(id, &p).unwrap();
    let second = lock(&RUNNING).get(id).cloned().unwrap();
    assert!(!Arc::ptr_eq(&first, &second));
    assert!(running(id));
    assert!(crate::base_hold::base_holder(id).is_some());
    stop(id).unwrap();
    wait_for(|| !running(id) && crate::base_hold::base_holder(id).is_none());
}

#[test]
fn restart_is_refused_like_play() {
    let root = tempfile::tempdir().unwrap();
    let blank = project("restart-blank", root.path(), " ");
    assert_eq!(
        restart("restart-blank", &blank).unwrap_err(),
        "No play command set for this project."
    );
    let id = "restart-vs-shoot";
    let root2 = tempfile::tempdir().unwrap();
    let p = project(id, root2.path(), LONG_RUNNING);
    let _shoot = crate::base_hold::hold_base(id, "a shoot").unwrap();
    assert_eq!(
        restart(id, &p).unwrap_err(),
        "The base checkout is busy: a shoot is using it."
    );
}
