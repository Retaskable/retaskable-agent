use std::path::PathBuf;

use super::*;
use crate::view::ProjectView;

fn view() -> AgentView {
    let v = AgentView {
        projects: vec![ProjectView {
            id: "one".into(),
            github_repo: "o/one".into(),
            backlog_path: "backlog".into(),
            repo_path: PathBuf::new(),
            worktrees_dir: PathBuf::new(),
            base_branch: String::new(),
            play_command: String::new(),
        }],
        version: "9.9.9".into(),
        ..AgentView::blank()
    };
    v.item_commands.gates().open_unrecorded("one");
    v
}

fn data_dir() -> PathBuf {
    std::env::temp_dir().join("retaskable-commands-test")
}

fn cmd(name: &str, args: Value) -> Command {
    Command {
        id: "c1".into(),
        name: name.into(),
        args,
        issued_by: "jc".into(),
        issued_at: now_ms(),
        expires_at: now_ms() + 60_000,
    }
}

#[test]
fn restart_acks_and_calls_exit_with_75() {
    let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let kept = calls.clone();
    let exit = move |code: i32| kept.lock().unwrap().push(code);
    let answer = run(
        &cmd("restart", json!({})),
        "one",
        &mut Seen::default(),
        &view(),
        &exit,
        &data_dir(),
    );
    assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
    assert_eq!(*calls.lock().unwrap(), vec![75]);
}

#[test]
fn pause_and_resume_need_computer_true_else_the_site_answered_it() {
    let v = view();
    let noop = |_: i32| {};
    let answer = run(
        &cmd("pause", json!({})),
        "one",
        &mut Seen::default(),
        &v,
        &noop,
        &data_dir(),
    );
    assert_eq!(answer["reason"], SITE_ANSWERS);
    assert_eq!(v.paused.reason("one"), None);

    let answer = run(
        &cmd("pause", json!({ "computer": true })),
        "one",
        &mut Seen::default(),
        &v,
        &noop,
        &data_dir(),
    );
    assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
    assert_eq!(
        v.paused.reason("one"),
        Some("Paused from retaskable.com by jc".to_string())
    );

    let answer = run(
        &cmd("resume", json!({ "computer": true })),
        "one",
        &mut Seen::default(),
        &v,
        &noop,
        &data_dir(),
    );
    assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
    assert_eq!(v.paused.reason("one"), None);
}

#[test]
fn a_replayed_id_is_refused_the_second_time() {
    let v = view();
    let noop = |_: i32| {};
    let mut seen = Seen::default();
    assert_eq!(
        run(&cmd("restart", json!({})), "one", &mut seen, &v, &noop, &data_dir())["type"],
        "ack"
    );
    let answer = run(&cmd("restart", json!({})), "one", &mut seen, &v, &noop, &data_dir());
    assert_eq!(answer["reason"], "Already received: a command runs once.");
}

#[test]
fn an_expired_command_is_refused() {
    let v = view();
    let noop = |_: i32| {};
    let mut old = cmd("restart", json!({}));
    old.issued_at = now_ms() - 61_000;
    old.expires_at = old.issued_at + 60_000;
    let answer = run(&old, "one", &mut Seen::default(), &v, &noop, &data_dir());
    assert!(answer["reason"].as_str().unwrap().starts_with("Expired"));
}

#[test]
fn before_setup_only_the_exempt_commands_run() {
    let v = view();
    let noop = |_: i32| {};
    let answer = run(
        &cmd("restart", json!({})),
        "",
        &mut Seen::default(),
        &v,
        &noop,
        &data_dir(),
    );
    assert_eq!(answer["reason"], "Finish setup first.");
}

#[test]
fn an_unported_command_is_named_in_its_nack() {
    let cmd = |name| cmd(name, json!({}));
    let v = view();
    let noop = |_: i32| {};
    let answer = run(&cmd("start_over"), "one", &mut Seen::default(), &v, &noop, &data_dir());
    assert_eq!(answer["reason"], "Not a command this agent takes yet: start_over");
}

#[test]
fn play_and_stop_nack_a_missing_lane_and_a_blank_command() {
    let v = view();
    let noop = |_: i32| {};
    for name in ["play", "stop"] {
        for args in [json!({}), json!({ "lane": -1 })] {
            let answer = run(&cmd(name, args), "one", &mut Seen::default(), &v, &noop, &data_dir());
            assert_eq!(answer["reason"], format!("{name} needs a lane."));
        }
    }
    let answer = run(
        &cmd("play", json!({ "lane": 0 })),
        "one",
        &mut Seen::default(),
        &v,
        &noop,
        &data_dir(),
    );
    assert_eq!(answer["reason"], "No play command set for this project.");
}

#[test]
fn play_and_stop_of_a_lane_are_queued_for_the_loop_and_refused_past_lanes() {
    let mut v = view();
    v.lanes = 2;
    v.projects[0].play_command = "game".into();
    let noop = |_: i32| {};
    let go = |name, lane, v: &AgentView| {
        run(
            &cmd(name, json!({ "lane": lane })),
            "one",
            &mut Seen::default(),
            v,
            &noop,
            &data_dir(),
        )
    };
    assert_eq!(
        go("play", 1, &v)["reason"],
        "This computer isn't running work (the orchestrator isn't in claim mode)."
    );
    v.item_commands.set_claim(true);
    assert_eq!(go("play", 3, &v)["reason"], "This computer has no lane 3.");
    assert_eq!(go("play", 2, &v)["type"], "ack");
    assert_eq!(go("stop", 1, &v)["type"], "ack");
    assert_eq!(
        v.item_commands.take_lane("one"),
        vec![
            ItemCommand::Play {
                project: "one".into(),
                lane: 2
            },
            ItemCommand::Stop {
                project: "one".into(),
                lane: 1
            },
        ]
    );
}

#[test]
fn the_gallery_names_are_no_command() {
    let v = view();
    let noop = |_: i32| {};
    for name in ["shoot", "gallery_index", "gallery_image", "gallery_feedback"] {
        let answer = run(
            &cmd(name, json!({ "gallery": "intro" })),
            "one",
            &mut Seen::default(),
            &v,
            &noop,
            &data_dir(),
        );
        assert_eq!(answer["reason"], format!("Not a command this agent takes yet: {name}"));
    }
}

fn claiming() -> AgentView {
    let v = view();
    v.item_commands.set_claim(true);
    v
}

fn go(v: &AgentView, args: Value, dir: &Path) -> Value {
    run(&cmd("continue", args), "one", &mut Seen::default(), v, &|_| {}, dir)
}

fn picture(bytes: usize) -> Value {
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD.encode(vec![7u8; bytes]);
    json!({ "type": "image/png", "data": data })
}

#[test]
fn continue_queues_the_notes_and_acks() {
    let v = claiming();
    let answer = go(&v, json!({ "id": "a", "notes": " Try again. " }), &data_dir());
    assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
    assert_eq!(
        v.item_commands.take("one"),
        vec![ItemCommand::Continue {
            project: "one".into(),
            item: "a".into(),
            notes: Some("Try again.".into())
        }]
    );
}

#[test]
fn continue_without_notes_queues_none() {
    let v = claiming();
    go(&v, json!({ "id": "a" }), &data_dir());
    let ItemCommand::Continue { notes, .. } = v.item_commands.take("one").remove(0) else {
        panic!("not a continue")
    };
    assert_eq!(notes, None);
}

#[test]
fn retry_queues_start_over_with_the_same_checks_and_pictures() {
    let v = claiming();
    let run = |args| {
        run(
            &cmd("retry", args),
            "one",
            &mut Seen::default(),
            &v,
            &|_| {},
            &data_dir(),
        )
    };
    assert_eq!(run(json!({ "id": "a", "notes": " Go again. " }))["type"], "ack");
    assert_eq!(
        v.item_commands.take("one"),
        vec![ItemCommand::StartOver {
            project: "one".into(),
            item: "a".into(),
            notes: Some("Go again.".into())
        }]
    );
    assert!(
        run(json!({ "id": " " }))["reason"]
            .as_str()
            .unwrap()
            .contains("Start over")
    );
    assert_eq!(run(json!({ "id": "a", "pictures": [picture(10)] }))["type"], "ack");
    let ItemCommand::StartOver { notes, .. } = v.item_commands.take("one").remove(0) else {
        panic!()
    };
    assert!(notes.unwrap().starts_with("Picture: "));
    let off = view();
    let answer = super::run(
        &cmd("retry", json!({ "id": "a" })),
        "one",
        &mut Seen::default(),
        &off,
        &|_| {},
        &data_dir(),
    );
    assert_eq!(answer["type"], "nack");
}

#[test]
fn continue_needs_an_id_and_claim_mode() {
    let v = claiming();
    assert!(
        go(&v, json!({}), &data_dir())["reason"]
            .as_str()
            .unwrap()
            .contains("id")
    );
    assert!(go(&v, json!({ "id": "  " }), &data_dir())["type"] == "nack");
    let off = view();
    let answer = go(&off, json!({ "id": "a" }), &data_dir());
    assert!(answer["reason"].as_str().unwrap().contains("claim mode"));
    assert!(off.item_commands.take("one").is_empty());
}

#[test]
fn continue_saves_pictures_under_attachments_and_adds_their_paths_after_the_notes() {
    let v = claiming();
    let dir = tempfile::tempdir().unwrap();
    let answer = go(
        &v,
        json!({ "id": "a", "notes": "See this.", "pictures": [picture(10), picture(20)] }),
        dir.path(),
    );
    assert_eq!(answer["type"], "ack");
    let ItemCommand::Continue { notes, .. } = v.item_commands.take("one").remove(0) else {
        panic!("not a continue")
    };
    let notes = notes.unwrap();
    let files: Vec<_> = std::fs::read_dir(dir.path().join("attachments")).unwrap().collect();
    assert_eq!(files.len(), 2);
    for f in files {
        let path = f.unwrap().path();
        assert!(notes.contains(&path.display().to_string()));
        assert_eq!(path.extension().unwrap(), "png");
    }
    assert!(notes.starts_with("See this.\nPicture: "));
}

#[test]
fn a_bad_or_oversized_picture_nacks_and_saves_nothing() {
    let v = claiming();
    let dir = tempfile::tempdir().unwrap();
    let too_many = json!([picture(1), picture(1), picture(1), picture(1), picture(1)]);
    let bad_type = json!([picture(1), { "type": "image/bmp", "data": "AAAA" }]);
    let bad_data = json!([picture(1), { "type": "image/png", "data": "***" }]);
    let big = json!([picture(1), picture((1 << 20) + 1)]);
    let in_all = json!([
        picture(1 << 20),
        picture(1 << 20),
        picture(1 << 20),
        picture(1 << 20),
        picture(1)
    ]);
    for pictures in [too_many, bad_type, bad_data, big, in_all] {
        let answer = go(&v, json!({ "id": "a", "pictures": pictures }), dir.path());
        assert_eq!(answer["type"], "nack", "{answer}");
    }
    assert!(!dir.path().join("attachments").exists());
    assert!(v.item_commands.take("one").is_empty());
}

#[test]
fn the_audit_log_holds_a_pictures_size_not_its_bytes() {
    let args = json!({ "id": "a", "pictures": [{ "type": "image/png", "data": "AAAA" }] });
    assert_eq!(
        audit_args("continue", &args)["pictures"],
        json!({ "count": 1, "bytes": 4 })
    );
}

#[test]
fn draft_files_answers_from_the_drafts_worktree_and_is_audited() {
    use crate::test_support::{git, repo_with_origin, run};
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(tmp.path());
    let worktrees = tmp.path().join("worktrees");
    let wt = crate::draft_worktree::path(&worktrees, "7");
    run(git(&repo).args(["worktree", "add", "--quiet", "-b", "spec-7"]).arg(&wt));
    std::fs::write(wt.join("new.md"), "# New\n").unwrap();
    run(git(&wt).args(["add", "new.md"]));
    run(git(&wt).args(["commit", "--quiet", "-m", "draft"]));
    let mut v = view();
    v.projects[0].worktrees_dir = worktrees;
    v.projects[0].base_branch = "main".into();
    let noop = |_: i32| {};
    let c = cmd("draft_files", json!({ "id": "7" }));
    let answer = run_cmd(&c, &v, &noop);
    assert_eq!(answer["type"], "ack", "{answer}");
    assert_eq!(
        answer["answer"]["files"],
        json!([{ "path": "new.md", "content": "# New\n" }])
    );

    audit(tmp.path(), &c, &answer, "wss://x");
    let log = std::fs::read_to_string(tmp.path().join("remote.log")).unwrap();
    assert!(log.contains("\"name\":\"draft_files\"") && log.contains("\"outcome\":\"ack\""));
    assert!(!log.contains("# New"));
}

#[test]
fn draft_edit_commits_and_pushes() {
    use crate::test_support::{git, repo_with_origin, run};
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(tmp.path());
    let worktrees = tmp.path().join("worktrees");
    let wt = crate::draft_worktree::path(&worktrees, "7");
    run(git(&repo)
        .args(["worktree", "add", "--quiet", "-b", "retaskable/spec-7-x"])
        .arg(&wt));
    let mut v = view();
    v.projects[0].repo_path = repo;
    v.projects[0].worktrees_dir = worktrees;
    v.projects[0].base_branch = "main".into();
    let noop = |_: i32| {};
    let edit = run_cmd(
        &cmd("draft_edit", json!({ "id": "7", "path": "n.md", "content": "hi\n" })),
        &v,
        &noop,
    );
    assert_eq!(edit["type"], "ack", "{edit}");
    assert_eq!(edit["answer"]["files"], json!([{ "path": "n.md", "content": "hi\n" }]));
    let missing = run_cmd(&cmd("draft_edit", json!({ "id": "7" })), &v, &noop);
    assert_eq!(missing["type"], "nack");
}

mod approve;
mod more;
mod setup_check;
use more::run_cmd;

fn logged(name: &str, args: Value) -> (String, Value) {
    let dir = tempfile::tempdir().unwrap();
    let c = cmd(name, args);
    audit(dir.path(), &c, &json!({ "type": "ack", "id": "c1" }), "wss://x");
    let log = std::fs::read_to_string(dir.path().join("remote.log")).unwrap();
    let line: Value = serde_json::from_str(log.trim()).unwrap();
    (log, line["args"].clone())
}

const SECRET: &str = "hunter2-é";

#[test]
fn continue_retry_and_playtest_log_notes_by_size() {
    for name in ["continue", "retry", "playtest"] {
        let args = json!({ "id": "a", "notes": SECRET, "pictures": [{ "type": "image/png", "data": "PICDATA" }],
            "pictureIds": ["p1", "p2"] });
        let (log, a) = logged(name, args);
        assert!(
            !log.contains(SECRET) && !log.contains("PICDATA") && !log.contains("p1"),
            "{name}: {log}"
        );
        assert_eq!(a["notes"], json!({ "bytes": SECRET.len() }));
        assert_eq!(a["pictureIds"], 2);
        assert_eq!(a["pictures"], json!({ "count": 1, "bytes": 7 }));
        assert_eq!(a["id"], "a");
    }
}

#[test]
fn draft_edit_logs_its_content_by_size_and_keeps_its_path() {
    let (log, a) = logged(
        "draft_edit",
        json!({ "id": "d", "path": "a.md", "branch": "b", "content": SECRET }),
    );
    assert!(!log.contains(SECRET), "{log}");
    assert_eq!(
        a,
        json!({ "id": "d", "path": "a.md", "branch": "b", "content": { "bytes": SECRET.len() } })
    );
}

#[test]
fn a_typed_value_that_is_not_a_string_is_sized_zero() {
    let (_, a) = logged("continue", json!({ "id": "a", "notes": { "secret": SECRET } }));
    assert_eq!(a["notes"], json!({ "bytes": 0 }));
}

#[test]
fn a_command_with_no_entry_logs_its_args_unchanged() {
    let (_, a) = logged("skip", json!({ "id": "a" }));
    assert_eq!(a, json!({ "id": "a" }));
}

#[test]
fn resume_computer_leaves_the_stores_pause_alone() {
    let v = view();
    let noop = |_: i32| {};
    v.store_paused
        .set("one", crate::view::StorePause::Paused("Paused for everyone".into()));
    run(
        &cmd("resume", json!({ "computer": true })),
        "one",
        &mut Seen::default(),
        &v,
        &noop,
        &data_dir(),
    );
    assert_eq!(
        v.store_paused.get("one"),
        crate::view::StorePause::Paused("Paused for everyone".into())
    );
}
