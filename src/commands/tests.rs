use std::path::PathBuf;

use super::*;
use crate::view::ProjectView;

fn view() -> AgentView {
    AgentView {
        machine: "pc".into(),
        tags: vec![],
        projects: vec![ProjectView {
            id: "one".into(),
            github_repo: "o/one".into(),
            backlog_path: "backlog".into(),
            repo_path: PathBuf::new(),
            worktrees_dir: PathBuf::new(),
            base_branch: String::new(),
            galleries: vec![],
            play_command: String::new(),
        }],
        version: "9.9.9".into(),
        lanes: 1,
        chore_lane: None,
        lane_sizes: Default::default(),
        paused: Default::default(),
        item_commands: Default::default(),
        work: Default::default(),
        restart: Default::default(),
    }
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
    let answer = run(
        &cmd("setup_check", json!({})),
        "",
        &mut Seen::default(),
        &v,
        &noop,
        &data_dir(),
    );
    assert_eq!(answer["reason"], "Not a command this agent takes yet: setup_check");
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
fn shoot_nacks_an_unknown_gallery() {
    let v = view();
    let noop = |_: i32| {};
    let answer = run(
        &cmd("shoot", json!({ "gallery": "intro" })),
        "one",
        &mut Seen::default(),
        &v,
        &noop,
        &data_dir(),
    );
    assert_eq!(answer["reason"], "No gallery called intro.");
}

#[test]
fn shoot_nacks_a_gallery_with_no_command_set() {
    let mut v = view();
    v.projects[0].galleries = vec![crate::config::GalleryConfig {
        name: "Intro".into(),
        command: String::new(),
        out_dir: "shots".into(),
    }];
    let noop = |_: i32| {};
    let answer = run(
        &cmd("shoot", json!({ "gallery": "intro" })),
        "one",
        &mut Seen::default(),
        &v,
        &noop,
        &data_dir(),
    );
    assert_eq!(answer["reason"], "No command set for this gallery.");
}

#[cfg(windows)]
fn copy_fixture_command() -> String {
    "mkdir shots && copy fixture.png shots\\01-first.png".to_string()
}
#[cfg(not(windows))]
fn copy_fixture_command() -> String {
    "mkdir -p shots && cp fixture.png shots/01-first.png".to_string()
}

#[test]
fn shoot_acks_at_once_and_runs_the_gallery_in_the_background() {
    let root = tempfile::tempdir().unwrap();
    let repo = crate::test_support::repo_with_origin(root.path());
    image::DynamicImage::ImageRgb8(image::RgbImage::new(100, 20))
        .save(repo.join("fixture.png"))
        .unwrap();
    crate::test_support::run(crate::test_support::git(&repo).args(["add", "fixture.png"]));
    crate::test_support::run(crate::test_support::git(&repo).args(["commit", "--quiet", "-m", "fixture"]));
    crate::test_support::run(crate::test_support::git(&repo).args(["push", "--quiet", "origin", "main"]));

    let mut v = view();
    v.projects[0].id = "commands-shoot-happy-path".into();
    v.projects[0].repo_path = repo.clone();
    v.projects[0].worktrees_dir = root.path().join("worktrees");
    v.projects[0].base_branch = "main".into();
    v.projects[0].galleries = vec![crate::config::GalleryConfig {
        name: "Models".into(),
        command: copy_fixture_command(),
        out_dir: "shots".into(),
    }];
    let noop = |_: i32| {};
    let dir = root.path().join("data");

    let answer = run(
        &cmd("shoot", json!({ "gallery": "models" })),
        "commands-shoot-happy-path",
        &mut Seen::default(),
        &v,
        &noop,
        &dir,
    );
    assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));

    let index = dir.join("projects/commands-shoot-happy-path/galleries/models/index.json");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    while !index.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(index.exists());
    assert_eq!(
        galleries::status("commands-shoot-happy-path", "models"),
        galleries::GalleryStatus::Idle
    );
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
    assert_eq!(audit_args(&args)["pictures"], json!({ "count": 1, "bytes": 4 }));
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

#[test]
fn draft_files_nacks_an_unknown_draft() {
    let tmp = tempfile::tempdir().unwrap();
    let mut v = view();
    v.projects[0].worktrees_dir = tmp.path().to_path_buf();
    let noop = |_: i32| {};
    for args in [json!({ "id": "9" }), json!({}), json!({ "id": "../x" })] {
        let answer = run_cmd(&cmd("draft_files", args), &v, &noop);
        assert_eq!(answer["type"], "nack", "{answer}");
    }
}

#[test]
fn approve_and_merge_into_main_are_queued_for_the_loop() {
    let v = view();
    let noop = |_: i32| {};
    let args = json!({ "draftId": "d1" });
    assert_eq!(run_cmd(&cmd("draft_approve", args.clone()), &v, &noop)["type"], "nack");
    v.item_commands.set_claim(true);
    assert_eq!(run_cmd(&cmd("draft_approve", args.clone()), &v, &noop)["type"], "ack");
    assert_eq!(run_cmd(&cmd("merge_pr", args.clone()), &v, &noop)["type"], "ack");
    assert_eq!(run_cmd(&cmd("draft_discard", args), &v, &noop)["type"], "ack");
    assert_eq!(run_cmd(&cmd("draft_discard", json!({})), &v, &noop)["type"], "nack");
    assert_eq!(run_cmd(&cmd("draft_approve", json!({})), &v, &noop)["type"], "nack");
    assert_eq!(
        run_cmd(&cmd("merge_pr", json!({ "itemId": "x" })), &v, &noop)["type"],
        "nack"
    );
    assert_eq!(
        v.item_commands.take_drafts("one"),
        [
            DraftCommand::Approve {
                project: "one".into(),
                draft: "d1".into()
            },
            DraftCommand::Merge {
                project: "one".into(),
                draft: "d1".into()
            },
            DraftCommand::Discard {
                project: "one".into(),
                draft: "d1".into()
            }
        ]
    );
}

fn run_cmd(c: &Command, v: &AgentView, exit: &Exit) -> Value {
    run(c, "one", &mut Seen::default(), v, exit, &data_dir())
}

#[test]
fn skip_queues_the_command_and_needs_an_id_and_claim_mode() {
    let v = claiming();
    let go = |v: &AgentView, args| run(&cmd("skip", args), "one", &mut Seen::default(), v, &|_| {}, &data_dir());
    assert_eq!(go(&v, json!({ "id": " a " }))["type"], "ack");
    assert_eq!(
        v.item_commands.take("one"),
        vec![ItemCommand::Skip {
            project: "one".into(),
            item: "a".into()
        }]
    );
    let no_id = go(&v, json!({}));
    assert_eq!(no_id["type"], "nack");
    assert!(no_id["reason"].as_str().unwrap().contains("Skip"));
    assert!(v.item_commands.take("one").is_empty());
    assert_eq!(go(&view(), json!({ "id": "a" }))["type"], "nack");
}

fn asking() -> AgentView {
    let v = claiming();
    v.item_commands.set_ask_on("one", true);
    v
}

fn ask_with(v: &AgentView, args: Value, dir: &Path) -> Value {
    run(&cmd("ask", args), "one", &mut Seen::default(), v, &|_| {}, dir)
}

#[test]
fn ask_queues_the_message_for_the_loop_and_acks() {
    let v = asking();
    let answer = ask_with(
        &v,
        json!({ "askId": "k1", "text": " why is step 5 stuck? ", "replyTo": "k0", "about": "item-a" }),
        &data_dir(),
    );
    assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
    assert_eq!(
        v.item_commands.take_asks("one"),
        vec![crate::view::AskCommand {
            project: "one".into(),
            ask_id: "k1".into(),
            text: "why is step 5 stuck?".into(),
            reply_to: Some("k0".into()),
            about: Some("item-a".into()),
            images_block: String::new(),
        }]
    );
}

#[test]
fn ask_saves_its_images_and_lists_their_paths_in_an_attached_images_block() {
    let v = asking();
    let dir = tempfile::tempdir().unwrap();
    ask_with(
        &v,
        json!({ "askId": "k1", "text": "look", "images": [picture(10)] }),
        dir.path(),
    );
    let ask = v.item_commands.take_asks("one").remove(0);
    let saved = std::fs::read_dir(dir.path().join("attachments")).unwrap().count();
    assert_eq!(saved, 1);
    assert!(ask.images_block.starts_with("Attached images"));
    assert!(ask.images_block.contains("attachments"));
    assert!(ask.images_block.contains(".png"));
}

#[test]
fn ask_checks_its_arguments() {
    let v = asking();
    let nack_of = |args: Value| {
        ask_with(&v, args, &data_dir())["reason"]
            .as_str()
            .unwrap_or("")
            .to_string()
    };
    assert_eq!(nack_of(json!({ "text": "hi" })), "ask needs its askId.");
    assert_eq!(nack_of(json!({ "askId": "  ", "text": "hi" })), "ask needs its askId.");
    assert_eq!(nack_of(json!({ "askId": "k" })), "ask needs the message's text.");
    assert_eq!(
        nack_of(json!({ "askId": "k", "text": " " })),
        "ask needs the message's text."
    );
    assert_eq!(
        nack_of(json!({ "askId": "k", "text": "hi", "replyTo": 3 })),
        "ask's replyTo must be text."
    );
    assert_eq!(
        nack_of(json!({ "askId": "k", "text": "hi", "images": "x" })),
        "pictures must be a list."
    );
    assert!(
        nack_of(json!({ "askId": "k", "text": "hi", "images": [json!({"type": "text/html", "data": "x"})] }))
            .contains("isn't a PNG")
    );
    assert!(v.item_commands.take_asks("one").is_empty());
}

#[test]
fn ask_is_refused_outside_claim_mode() {
    let v = view();
    let answer = ask_with(&v, json!({ "askId": "k", "text": "hi" }), &data_dir());
    assert_eq!(answer["type"], "nack");
    assert!(v.item_commands.take_asks("one").is_empty());
}

#[test]
fn ask_is_refused_while_the_flag_is_off() {
    let v = claiming();
    let answer = ask_with(&v, json!({ "askId": "k", "text": "hi" }), &data_dir());
    assert_eq!(answer["type"], "nack");
    assert_eq!(answer["reason"], "Not a command this agent takes yet: ask");
    assert!(v.item_commands.take_asks("one").is_empty());
}

fn idea_accept(v: &AgentView, args: Value) -> Value {
    run(
        &cmd("idea_accept", args),
        "one",
        &mut Seen::default(),
        v,
        &|_| {},
        &data_dir(),
    )
}

#[test]
fn idea_accept_is_queued_for_the_loop_behind_the_flag_and_answered_later() {
    let v = claiming();
    let off = idea_accept(&v, json!({ "idea": "i1" }));
    assert_eq!(off["type"], "nack");
    assert!(off["reason"].as_str().unwrap().contains("idea_accept"));
    v.item_commands.set_flag("one", "ideas-session", true);
    assert_eq!(
        idea_accept(&v, json!({ "idea": "i1" }))["type"],
        crate::commands::PENDING
    );
    let queued = v.item_commands.take_ideas("one");
    assert_eq!(queued.len(), 1);
    assert_eq!((queued[0].idea.as_str(), queued[0].by.as_str()), ("i1", "jc"));
    assert_eq!(idea_accept(&v, json!({ "idea": " " }))["type"], "nack");
    assert_eq!(idea_accept(&v, json!({}))["type"], "nack");
    assert!(v.item_commands.take_ideas("one").is_empty());
}

#[test]
fn idea_accept_needs_claim_mode_and_not_now_and_never_are_the_sites() {
    let v = view();
    v.item_commands.set_flag("one", "ideas-session", true);
    assert_eq!(idea_accept(&v, json!({ "idea": "i1" }))["type"], "nack");
    for name in ["idea_later", "idea_never"] {
        let answer = run(
            &cmd(name, json!({ "idea": "i1" })),
            "one",
            &mut Seen::default(),
            &v,
            &|_| {},
            &data_dir(),
        );
        assert_eq!(answer["type"], "nack");
        assert!(answer["reason"].as_str().unwrap().contains("answers this one itself"));
    }
}

#[test]
fn a_settled_answer_goes_up_the_projects_open_connection() {
    let v = claiming();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    v.item_commands.answer("one", json!({ "type": "ack", "id": "lost" }));
    v.item_commands.set_out("one", Some(tx));
    v.item_commands.answer("one", json!({ "type": "ack", "id": "c1" }));
    assert_eq!(rx.try_recv().unwrap()["id"], "c1");
    v.item_commands.set_out("one", None);
    v.item_commands.answer("one", json!({ "type": "ack", "id": "c2" }));
    assert!(rx.try_recv().is_err());
}

#[test]
fn ideas_generate_queues_a_round_and_nacks_when_paused_or_not_set_up() {
    let v = view();
    let noop = |_: i32| {};
    let go = |args: Value, project: &str, v: &AgentView| {
        run(
            &cmd("ideas_generate", args),
            project,
            &mut Seen::default(),
            v,
            &noop,
            &data_dir(),
        )
    };
    assert_eq!(go(json!({}), "one", &v)["type"], "ack");
    assert_eq!(go(json!({ "fresh": true }), "one", &v)["type"], "ack");
    assert_eq!(
        v.item_commands.take("one"),
        vec![
            ItemCommand::Ideas {
                project: "one".into(),
                fresh: false
            },
            ItemCommand::Ideas {
                project: "one".into(),
                fresh: true
            },
        ]
    );
    assert_eq!(go(json!({}), "", &v)["reason"], "Finish setup first.");
    v.paused.set("one", Some("test".into()));
    assert_eq!(
        go(json!({}), "one", &v)["reason"],
        "The project is paused: resume it first."
    );
    assert!(v.item_commands.take("one").is_empty());
}
