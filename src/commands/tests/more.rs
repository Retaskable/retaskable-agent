use super::*;
use crate::view::DraftCommand;

#[test]
fn the_design_presses_are_rows_now_and_refused_pushed() {
    let v = view();
    v.item_commands.set_runs(true);
    let noop = |_: i32| {};
    for name in ["draft_skip_design", "board_design_write"] {
        let answer = run_cmd(&cmd(name, json!({ "draftId": "d1" })), &v, &noop);
        assert_eq!(answer["type"], "nack", "{name}");
        assert_eq!(answer["reason"], PULLED_REFUSAL, "{name}");
    }
    assert!(v.item_commands.take_drafts("one").is_empty(), "nothing was queued");
}

#[test]
fn what_the_hub_answers_itself_is_refused_in_its_words_when_pushed() {
    let v = view();
    let noop = |_: i32| {};
    for name in [
        "assign",
        "audience",
        "audiences",
        "chore_run",
        "release_edit",
        "release_clear",
        "release_save",
        "flag_set",
        "draft_step",
        "free",
    ] {
        assert_eq!(
            run_cmd(&cmd(name, json!({})), &v, &noop)["reason"],
            SITE_ANSWERS,
            "{name}"
        );
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
    // An item's is the loop's too (`sweep2`); none, or two, named is refused.
    assert_eq!(
        run_cmd(&cmd("merge_pr", json!({})), &v, &noop)["reason"],
        "Name one item, one draft, or the pull request into main to merge."
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

pub(super) fn run_cmd(c: &Command, v: &AgentView, exit: &Exit) -> Value {
    run(c, "one", &mut Seen::default(), v, exit, &data_dir())
}

#[test]
fn skip_queues_the_command_and_needs_an_id_and_claim_mode() {
    let v = claiming();
    let go = |v: &AgentView, args| run(&cmd("skip", args), "one", &mut Seen::default(), v, &|_| {}, &data_dir());
    assert_eq!(go(&v, json!({ "itemId": " a " }))["type"], "ack");
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
    assert_eq!(go(&view(), json!({ "itemId": "a" }))["type"], "nack");
    // The old shape, `id`, is not what the hub sends.
    assert_eq!(go(&v, json!({ "id": "a" }))["type"], "nack");
}

#[test]
fn the_ask_command_is_gone_the_chat_asks_through_intent_rows() {
    let answer = run(
        &cmd("ask", json!({ "askId": "k", "text": "hi" })),
        "one",
        &mut Seen::default(),
        &claiming(),
        &|_| {},
        &data_dir(),
    );
    assert_eq!(answer["type"], "nack");
}

fn idea_accept_as(v: &AgentView, name: &str, args: Value) -> Value {
    run(&cmd(name, args), "one", &mut Seen::default(), v, &|_| {}, &data_dir())
}

#[test]
fn the_requests_the_site_writes_as_rows_are_refused_pushed() {
    // `draft_spec`, `idea_accept` and the rest of `INTENT_COMMANDS` reach this computer as intent rows, never pushed.
    for (name, args) in [
        ("idea_accept", json!({ "idea": "i1" })),
        ("draft_spec", json!({ "request": "Add a dark mode" })),
        ("take_over", json!({ "itemId": "7" })),
        ("finish_steps", json!({ "itemId": "7" })),
        ("sync_push", json!({})),
        ("team_sync", json!({})),
        ("main_pr", json!({})),
        ("ci_fix", json!({ "pr": 5, "sha": "abc1234" })),
    ] {
        let answer = idea_accept_as(&claiming(), name, args);
        assert_eq!(answer["type"], "nack", "{name}");
        assert_eq!(
            answer["reason"],
            "That is a request on retaskable.com now: this computer takes it from there, not as a pushed command.",
            "{name}"
        );
    }
}

#[test]
fn not_now_and_never_are_the_sites() {
    let v = view();
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
fn ideas_generate_is_nacked_like_any_command_this_agent_doesnt_take() {
    let v = claiming();
    let answer = run(
        &cmd("ideas_generate", json!({})),
        "one",
        &mut Seen::default(),
        &v,
        &|_: i32| {},
        &data_dir(),
    );
    assert_eq!(answer["type"], "nack");
    assert!(v.item_commands.take("one").is_empty());
}

const SEED: &str = r#"{ "version": 2, "projects": [], "pollIntervalSec": 30, "custom": true }"#;

/// A view whose settings read `dir`, with the new-hello signal's receiver.
fn settings_view(dir: &Path) -> (AgentView, tokio::sync::watch::Receiver<u64>) {
    let mut v = view();
    v.settings = crate::hello_settings::HelloSettings::new(dir.to_path_buf(), crate::env::Env::empty());
    let mut seen = v.settings.subscribe();
    seen.mark_unchanged();
    (v, seen)
}

fn send(v: &AgentView, name: &str, args: Value) -> Value {
    run(&cmd(name, args), "one", &mut Seen::default(), v, &|_| {}, &data_dir())
}

/// The channel is retired (agent-settings.md section 2.1; the owner's settings are the store's `pool_settings`): the
/// command is one this agent doesn't take, whatever it carries, and writes nothing or wakes the hello.
#[test]
fn agent_settings_is_not_a_command_this_agent_takes_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.json");
    std::fs::write(&file, SEED).unwrap();
    let (v, seen) = settings_view(dir.path());
    for args in [json!({ "dailyUsd": 12 }), json!({}), json!({ "pollIntervalSec": 5 })] {
        let answer = send(&v, "agent_settings", args);
        assert_eq!(answer["type"], "nack");
        assert_eq!(answer["reason"], "Not a command this agent takes yet: agent_settings");
    }
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(kept, serde_json::from_str::<Value>(SEED).unwrap());
    assert!(!seen.has_changed().unwrap());
}

#[test]
fn computer_tags_and_settings_request_are_refused_and_change_no_setting() {
    let dir = tempfile::tempdir().unwrap();
    let (v, seen) = settings_view(dir.path());
    let answer = send(&v, "computer_tags", json!({ "tags": ["a"] }));
    assert_eq!(answer["reason"], "Not a command this agent takes yet: computer_tags");
    let answer = send(&v, "settings_request", json!({ "tags": ["a"] }));
    assert_eq!(answer["type"], "nack");
    assert_eq!(
        answer["reason"],
        "tags is the project's, kept on retaskable.com: change it in Project settings there."
    );
    assert!(!dir.path().join("config.json").exists());
    assert!(!seen.has_changed().unwrap());
}

fn playtest_view() -> AgentView {
    claiming()
}

fn answer_playtest(v: &AgentView, args: Value, dir: &Path) -> Value {
    run(&cmd("playtest", args), "one", &mut Seen::default(), v, &|_| {}, dir)
}

#[test]
fn playtest_queues_the_answer_with_notes_and_the_pictures_to_fetch() {
    let v = playtest_view();
    let d = data_dir();
    let id = format!("{}.png", "a".repeat(36));
    let args = json!({ "itemId": "a", "result": "fail", "notes": " Broken. ", "pictureIds": [id] });
    assert_eq!(answer_playtest(&v, args, &d), json!({ "type": "pending", "id": "c1" }));
    let ItemCommand::Playtest {
        id: item,
        passed,
        notes,
        attached,
        ..
    } = v.item_commands.take("one").remove(0)
    else {
        panic!("not a playtest")
    };
    assert_eq!((item.as_str(), passed), ("a", false));
    assert_eq!(notes.as_deref(), Some("Broken."));
    assert_eq!(attached.unwrap().ids, vec![format!("{}.png", "a".repeat(36))]);
    let pass = answer_playtest(&v, json!({ "itemId": "a", "result": "pass" }), &d);
    assert_eq!(pass["type"], "ack");
    let ItemCommand::Playtest {
        passed,
        notes,
        attached,
        ..
    } = v.item_commands.take("one").remove(0)
    else {
        panic!("not a playtest")
    };
    assert!(passed && notes.is_none() && attached.is_none());
}

#[test]
fn playtest_takes_item_id_and_keeps_merged_at() {
    let v = playtest_view();
    let d = data_dir();
    let args = json!({ "itemId": "a", "result": "pass", "mergedAt": " 2026-01-02 " });
    assert_eq!(answer_playtest(&v, args, &d)["type"], "ack");
    let ItemCommand::Playtest { id, merged_at, .. } = v.item_commands.take("one").remove(0) else {
        panic!("not a playtest")
    };
    assert_eq!((id.as_str(), merged_at.as_deref()), ("a", Some("2026-01-02")));
    assert_eq!(
        answer_playtest(&v, json!({ "itemId": "a", "result": "pass", "mergedAt": " " }), &d)["type"],
        "ack"
    );
    let ItemCommand::Playtest { merged_at, .. } = v.item_commands.take("one").remove(0) else {
        panic!("not a playtest")
    };
    assert!(merged_at.is_none());
}

#[test]
fn playtest_is_nacked_for_bad_arguments_and_modes() {
    let v = playtest_view();
    let d = data_dir();
    for args in [
        json!({ "result": "pass" }),
        json!({ "itemId": "  ", "result": "pass" }),
        json!({ "itemId": "a" }),
        json!({ "itemId": "a", "result": "maybe" }),
        json!({ "id": "a", "result": "pass" }),
    ] {
        assert_eq!(answer_playtest(&v, args, &d)["type"], "nack");
    }
    let off = view();
    let answer = answer_playtest(&off, json!({ "itemId": "a", "result": "pass" }), &d);
    assert!(answer["reason"].as_str().unwrap().contains("claim or runs mode"));
    assert!(v.item_commands.take("one").is_empty() && off.item_commands.take("one").is_empty());
}

#[test]
fn playtest_is_taken_in_runs_mode_too_for_a_try_outs_answer() {
    let v = view();
    v.item_commands.set_runs(true);
    let d = data_dir();
    assert_eq!(
        answer_playtest(&v, json!({ "itemId": "7", "result": "pass" }), &d)["type"],
        "ack"
    );
    assert!(matches!(
        v.item_commands.take_playtests("one").as_slice(),
        [ItemCommand::Playtest { id, passed: true, .. }] if id == "7"
    ));
}

#[test]
fn playtest_with_a_bad_picture_id_queues_nothing() {
    let v = playtest_view();
    let args = json!({ "itemId": "a", "result": "pass", "pictureIds": ["../x.png"] });
    let answer = answer_playtest(&v, args, &data_dir());
    assert_eq!(answer["type"], "nack");
    assert_eq!(answer["reason"], "No attached image ../x.png.");
    assert!(v.item_commands.take("one").is_empty());
}

#[test]
fn playtest_is_nacked_for_an_item_held_in_another_state() {
    let v = playtest_view();
    let held = |id: &str, state: &str| crate::orchestrator::live::HeldItem {
        id: id.into(),
        title: String::new(),
        state: state.into(),
        lane: None,
        branch: String::new(),
        pr_url: None,
        attempt: 0,
        chore: false,
        expert: None,
    };
    v.live
        .set_items("one", vec![held("a", "building"), held("b", "awaiting_playtest")]);
    let d = data_dir();
    assert_eq!(
        answer_playtest(&v, json!({ "itemId": "a", "result": "pass" }), &d)["type"],
        "nack"
    );
    assert_eq!(
        answer_playtest(&v, json!({ "itemId": "b", "result": "pass" }), &d)["type"],
        "ack"
    );
    assert_eq!(
        answer_playtest(&v, json!({ "itemId": "other", "result": "pass" }), &d)["type"],
        "ack"
    );
    assert_eq!(v.item_commands.take("one").len(), 2);
}

#[test]
fn restart_play_and_stop_play_take_lane_0_and_nack_the_rest() {
    let mut v = view();
    v.lanes = 2;
    let noop = |_: i32| {};
    let go = |name, args, v: &AgentView| run(&cmd(name, args), "one", &mut Seen::default(), v, &noop, &data_dir());
    assert_eq!(go("restart_play", json!({}), &v)["reason"], "Name an item or a lane.");
    assert_eq!(
        go("restart_play", json!({ "lane": 3 }), &v)["reason"],
        "This computer has no lane 3."
    );
    assert_eq!(go("restart_play", json!({ "lane": 1 }), &v)["type"], "nack");
    assert_eq!(
        go("restart_play", json!({ "lane": 0 }), &v)["reason"],
        "No play command set for this project."
    );
    // No lane is Play main: nothing runs it here.
    assert_eq!(go("stop_play", json!({}), &v)["reason"], "Play main isn't running.");
}

#[test]
fn stop_play_stops_play_main_like_stop() {
    let root = tempfile::tempdir().unwrap();
    let id = "cmd-stop-play";
    let mut v = view();
    v.projects[0].id = id.into();
    v.item_commands.gates().open_unrecorded(id);
    v.projects[0].repo_path = crate::test_support::repo_with_origin(root.path());
    v.projects[0].worktrees_dir = root.path().join("worktrees");
    v.projects[0].base_branch = "main".into();
    #[cfg(windows)]
    let long = "ping -n 60 127.0.0.1 >nul";
    #[cfg(not(windows))]
    let long = "sleep 60";
    v.projects[0].play_command = long.into();
    let noop = |_: i32| {};
    let go = |name, v: &AgentView| {
        run(
            &cmd(name, json!({ "lane": 0 })),
            id,
            &mut Seen::default(),
            v,
            &noop,
            &data_dir(),
        )
    };
    assert_eq!(go("stop_play", &v)["reason"], "Play main isn't running.");
    assert_eq!(go("play", &v)["type"], "ack");
    assert!(crate::play::running(id));
    assert_eq!(go("stop_play", &v)["type"], "ack");
    assert!(!crate::play::running(id));
}

#[test]
fn restart_play_of_a_lane_is_queued_in_claim_mode_and_nacked_otherwise() {
    let mut v = view();
    v.lanes = 2;
    v.projects[0].play_command = "the-game".into();
    let go = |v: &AgentView, lane| {
        run(
            &cmd("restart_play", json!({ "lane": lane })),
            "one",
            &mut Seen::default(),
            v,
            &|_| {},
            &data_dir(),
        )
    };
    assert_eq!(go(&v, 1)["type"], "nack", "not in claim mode");
    assert!(v.item_commands.take_lane("one").is_empty());
    v.item_commands.set_claim(true);
    assert_eq!(go(&v, 2)["type"], "ack");
    assert!(matches!(
        v.item_commands.take_lane("one").as_slice(),
        [ItemCommand::RestartPlay { lane: 2, .. }]
    ));
    assert_eq!(go(&v, 3)["reason"], "This computer has no lane 3.");
    v.projects[0].play_command = " ".into();
    assert_eq!(go(&v, 1)["reason"], "No play command set for this project.");
    assert!(v.item_commands.take_lane("one").is_empty());
}

#[test]
fn phone_play_is_refused_for_a_project_with_no_phone_section_and_phone_stop_is_always_taken() {
    let v = view();
    let noop = |_: i32| {};
    let play = run_cmd(&cmd("phone_play", json!({ "serial": "0A1B" })), &v, &noop);
    assert_eq!(play["reason"], "Play on phone isn't set up.");
    let no_serial = run_cmd(&cmd("phone_play", json!({})), &v, &noop);
    assert_eq!(
        no_serial["reason"],
        "That phone isn't connected with USB debugging allowed."
    );
    assert_eq!(run_cmd(&cmd("phone_stop", json!({})), &v, &noop)["type"], "ack");
}

#[test]
fn release_status_is_queued_for_the_loop_in_claim_and_runs_mode_and_the_rest_is_refused_in_words() {
    use crate::view::ReadCommand;
    let noop = |_: i32| {};
    let args = json!({ "part": "note", "id": "2026-09-30-notes.md" });
    let idle = view();
    assert_eq!(
        run_cmd(&cmd("release_status", args.clone()), &idle, &noop)["type"],
        "nack"
    );
    for v in [claiming(), {
        let v = view();
        v.item_commands.set_runs(true);
        v
    }] {
        assert_eq!(
            run_cmd(&cmd("release_status", args.clone()), &v, &noop),
            json!({ "type": "pending", "id": "c1" })
        );
        assert_eq!(
            v.item_commands.take_reads("one"),
            [ReadCommand {
                project: "one".into(),
                command_id: "c1".into(),
                kind: "release_status".into(),
                args: args.clone()
            }]
        );
    }
    let v = claiming();
    for (name, reason) in [
        (
            "change_files",
            "This project's changes are on its code host: open them there.",
        ),
        (
            "ask",
            "Chat is answered by the Chat Agent, not by this computer's coding agent.",
        ),
        ("release_generate", SITE_ANSWERS),
    ] {
        assert_eq!(
            run_cmd(&cmd(name, json!({ "change": 4 })), &v, &noop)["reason"],
            reason,
            "{name}"
        );
    }
}

#[test]
fn settings_request_is_refused_in_the_words_the_ts_agent_uses() {
    let v = view();
    let noop = |_: i32| {};
    let reason = |args: Value| run_cmd(&cmd("settings_request", args), &v, &noop)["reason"].clone();
    assert_eq!(reason(json!({})), "Name a change.");
    assert_eq!(
        reason(json!({ "keepBranches": true })),
        "Only a project on a plain git remote can keep its changes' branches on this computer."
    );
    assert_eq!(
        reason(json!({ "playtests": "on" })),
        "playtests is the project's, kept on retaskable.com: change it in Project settings there."
    );
    assert_eq!(
        reason(json!({ "chores": "on", "playtests": "off" })),
        "chores, playtests are the project's, kept on retaskable.com: change them in Project settings there."
    );
    assert_eq!(
        reason(json!({ "lanes": 4 })),
        "lanes are the computer's, shared by all its projects: set them at the computer (its Settings page or RETASKABLE_LANES)."
    );
    let both = reason(json!({ "lanes": 4, "icon": null, "keepBranches": false }));
    assert!(
        both.as_str().unwrap().starts_with("icon is the project's, kept on retaskable.com: change it in Project settings there. The lanes are the computer's"),
        "{both}"
    );
}

#[tokio::test]
async fn backlog_link_starts_the_stores_device_flow_and_is_refused_without_the_agents_links() {
    use crate::link::fake_cloud;
    use crate::link::pending::{Links, Opener};
    let v = view();
    let noop = |_: i32| {};
    let refused = run_cmd(&cmd("backlog_link", json!({})), &v, &noop);
    assert_eq!(
        refused["reason"],
        "This agent can't link to the shared backlog from here."
    );

    let store = fake_cloud::spawn().await;
    let dir = tempfile::tempdir().unwrap();
    let opened = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = opened.clone();
    let opener: Opener = std::sync::Arc::new(move |url| {
        seen.lock().unwrap().push(url.to_string());
        Ok(())
    });
    let links = Links::with(
        crate::settings_server::test_keychain(dir.path()),
        reqwest::Client::new(),
        json!({ "machine": "pc", "os": "Linux", "agentVersion": "1.2.3" }),
        opener,
        std::time::Duration::from_millis(1),
    );
    v.backlog_link.set(links.clone(), store.base());
    let mut changes = links.subscribe();
    assert_eq!(run_cmd(&cmd("backlog_link", json!({})), &v, &noop)["type"], "ack");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while opened.lock().unwrap().is_empty() {
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the approval page was never opened");
    assert_eq!(
        *opened.lock().unwrap(),
        [format!("{}/link?code={}", store.base(), fake_cloud::USER_CODE)]
    );
    // A second ask while one waits answers the same and starts nothing new.
    let again = run_cmd(&cmd("backlog_link", json!({})), &v, &noop);
    assert_eq!(again["type"], "ack");
    assert_eq!(store.asked(), 1);
    store.approve();
}
