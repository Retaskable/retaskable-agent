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
            phone: None,
            organisation_id: None,
            code_host: "github".into(),
            code_host_ref: String::new(),
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
        let answer = run(
            &cmd(name, json!({ "lane": -1 })),
            "one",
            &mut Seen::default(),
            &v,
            &noop,
            &data_dir(),
        );
        assert_eq!(answer["reason"], format!("{name} needs a lane."));
    }
    // No lane is Play main, as in the TS agent (`lane ?? 0`): here there is no play command to run.
    let answer = run(
        &cmd("play", json!({})),
        "one",
        &mut Seen::default(),
        &v,
        &noop,
        &data_dir(),
    );
    assert_eq!(answer["reason"], "No play command set for this project.");
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
        "This computer isn't running work (the orchestrator isn't in claim or runs mode)."
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

/// A picture id as the store gives one: 36 characters of `[0-9a-f-]` and a picture extension.
fn picture_id(c: char) -> String {
    format!("{}.png", c.to_string().repeat(36))
}

#[test]
fn continue_queues_the_notes_and_acks() {
    let v = claiming();
    let answer = go(&v, json!({ "itemId": "a", "notes": " Try again. " }), &data_dir());
    assert_eq!(answer, json!({ "type": "ack", "id": "c1" }));
    assert_eq!(
        v.item_commands.take("one"),
        vec![ItemCommand::Continue {
            project: "one".into(),
            item: "a".into(),
            notes: Some("Try again.".into()),
            attached: None
        }]
    );
}

#[test]
fn continue_without_notes_queues_none() {
    let v = claiming();
    go(&v, json!({ "itemId": "a" }), &data_dir());
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
    assert_eq!(run(json!({ "itemId": "a", "notes": " Go again. " }))["type"], "ack");
    assert_eq!(
        v.item_commands.take("one"),
        vec![ItemCommand::StartOver {
            project: "one".into(),
            item: "a".into(),
            notes: Some("Go again.".into()),
            attached: None
        }]
    );
    assert!(
        run(json!({ "itemId": " " }))["reason"]
            .as_str()
            .unwrap()
            .contains("Start over")
    );
    let sent = run(json!({ "itemId": "a", "pictureIds": [picture_id('a')] }));
    assert_eq!(sent, json!({ "type": "pending", "id": "c1" }));
    let ItemCommand::StartOver { notes, attached, .. } = v.item_commands.take("one").remove(0) else {
        panic!()
    };
    assert_eq!(notes, None);
    assert_eq!(attached.unwrap().ids, vec![picture_id('a')]);
    let off = view();
    let answer = super::run(
        &cmd("retry", json!({ "itemId": "a" })),
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
    assert!(go(&v, json!({ "itemId": "  " }), &data_dir())["type"] == "nack");
    // `id` is not the hub's name for it.
    assert!(go(&v, json!({ "id": "a" }), &data_dir())["type"] == "nack");
    let off = view();
    let answer = go(&off, json!({ "itemId": "a" }), &data_dir());
    assert!(answer["reason"].as_str().unwrap().contains("claim mode"));
    assert!(off.item_commands.take("one").is_empty());
}

#[test]
fn continue_queues_its_picture_ids_to_fetch_and_answers_pending() {
    let v = claiming();
    let ids = json!([picture_id('a'), picture_id('b')]);
    let answer = go(
        &v,
        json!({ "itemId": "a", "notes": "See this.", "pictureIds": ids }),
        &data_dir(),
    );
    assert_eq!(answer, json!({ "type": "pending", "id": "c1" }));
    let ItemCommand::Continue { notes, attached, .. } = v.item_commands.take("one").remove(0) else {
        panic!("not a continue")
    };
    // The notes go up as they are: the loop adds the pictures' paths once it has fetched them.
    assert_eq!(notes.as_deref(), Some("See this."));
    let attached = attached.unwrap();
    assert_eq!(attached.command_id, "c1");
    assert_eq!(attached.ids, vec![picture_id('a'), picture_id('b')]);
}

#[test]
fn a_bad_or_too_many_picture_ids_nack_and_queue_nothing() {
    let v = claiming();
    let five: Vec<String> = "abcde".chars().map(picture_id).collect();
    for ids in [
        json!(five),
        json!([picture_id('a'), "../x.png"]),
        json!(["a.png"]),
        json!("a"),
        json!([7]),
    ] {
        let answer = go(&v, json!({ "itemId": "a", "pictureIds": ids }), &data_dir());
        assert_eq!(answer["type"], "nack", "{answer}");
    }
    assert!(v.item_commands.take("one").is_empty());
}

#[test]
fn the_reads_are_not_commands_this_agent_takes_pushed_and_a_drafts_files_are_answered_when_the_store_has_none() {
    let v = view();
    v.item_commands.set_claim(true);
    let noop = |_: i32| {};
    // A draft's files are a fact kept in the store, which the hub answers from; it still relays the command when the store has
    // none yet (`hubStoreCommands.ts`), and the loop answers it (`read_kinds::draft_files`). The reads are intent rows
    // (`PULLED`, as TS refuses them pushed).
    let files = run_cmd(&cmd("draft_files", json!({ "draftId": "7" })), &v, &noop);
    assert_eq!(files["type"], "pending", "{files}");
    let queued = v.item_commands.take_reads("one");
    assert_eq!(
        (queued[0].kind.as_str(), queued[0].args.clone()),
        ("draft_files", json!({ "draftId": "7" }))
    );
    for name in ["read_file", "issues_preview", "environments"] {
        let answer = run_cmd(&cmd(name, json!({})), &v, &noop);
        assert_eq!(answer["reason"], PULLED_REFUSAL, "{name}");
    }
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
    assert_eq!(
        edit["result"]["files"],
        json!([{ "path": "n.md", "content": "hi\n", "isNew": true }])
    );
    let missing = run_cmd(&cmd("draft_edit", json!({ "id": "7" })), &v, &noop);
    assert_eq!(missing["type"], "nack");
}

mod approve;
mod expert;
mod github;
mod link;
mod more;
mod runs_mode;
mod setup_check;
mod setup_tools;
mod sweep2;
mod wire;
use more::run_cmd;

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
