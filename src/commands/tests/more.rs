use super::*;

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

pub(super) fn run_cmd(c: &Command, v: &AgentView, exit: &Exit) -> Value {
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

const SEED: &str = r#"{ "version": 2, "projects": [], "pollIntervalSec": 30, "custom": true }"#;

/// A view whose settings read `dir`, with the new-hello signal's receiver.
fn settings_view(dir: &Path) -> (AgentView, tokio::sync::watch::Receiver<u64>) {
    let mut v = view();
    v.settings = crate::hello_settings::HelloSettings::new(
        dir.to_path_buf(),
        crate::env::Env::empty(),
        4700,
        std::sync::Arc::new(crate::flags::Flags::fixed(&["machine-settings"])),
    );
    let mut seen = v.settings.subscribe();
    seen.mark_unchanged();
    (v, seen)
}

fn send(v: &AgentView, name: &str, args: Value) -> Value {
    run(&cmd(name, args), "one", &mut Seen::default(), v, &|_| {}, &data_dir())
}

#[test]
fn agent_settings_is_saved_at_once_keeps_the_rest_and_fires_the_hello_signal() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.json");
    std::fs::write(&file, SEED).unwrap();
    let (v, seen) = settings_view(dir.path());
    let answer = send(&v, "agent_settings", json!({ "dailyUsd": 12 }));
    assert_eq!(answer["type"], "ack");
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["budget"]["dailyUsd"], 12.0);
    assert_eq!(saved["pollIntervalSec"], 30);
    assert_eq!(saved["custom"], true);
    assert!(seen.has_changed().unwrap());
}

#[test]
fn agent_settings_refuses_bad_values_empty_changes_and_computer_only_fields_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("config.json");
    std::fs::write(&file, SEED).unwrap();
    let (v, seen) = settings_view(dir.path());
    for args in [
        json!({ "dailyUsd": 100001 }),
        json!({ "concurrency": 0 }),
        json!({}),
        json!({ "pollIntervalSec": 5 }),
        json!({ "local": { "enabled": true } }),
        json!({ "tags": ["a"] }),
        json!({ "dailyUsd": 5, "tags": ["a"] }),
    ] {
        let answer = send(&v, "agent_settings", args.clone());
        assert_eq!(answer["type"], "nack", "{args}");
    }
    let answer = send(&v, "agent_settings", json!({ "pollIntervalSec": 5 }));
    assert!(
        answer["reason"]
            .as_str()
            .unwrap()
            .starts_with("Not a command this agent takes")
    );
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(kept, serde_json::from_str::<Value>(SEED).unwrap());
    assert!(!seen.has_changed().unwrap());
}

#[test]
fn computer_tags_and_settings_request_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (v, seen) = settings_view(dir.path());
    for name in ["computer_tags", "settings_request"] {
        let answer = send(&v, name, json!({ "tags": ["a"] }));
        assert_eq!(answer["type"], "nack", "{name}");
        assert_eq!(answer["reason"], format!("Not a command this agent takes yet: {name}"));
    }
    assert!(!dir.path().join("config.json").exists());
    assert!(!seen.has_changed().unwrap());
}

#[test]
fn agent_settings_is_an_unknown_command_while_the_flag_is_off_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut v = view();
    v.settings = crate::hello_settings::HelloSettings::new(
        dir.path().to_path_buf(),
        crate::env::Env::empty(),
        4700,
        std::sync::Arc::new(crate::flags::Flags::fixed(&[])),
    );
    let answer = send(&v, "agent_settings", json!({ "dailyUsd": 12 }));
    assert_eq!(answer["reason"], "Not a command this agent takes yet: agent_settings");
    assert!(!dir.path().join("config.json").exists());
}

fn playtest_view() -> AgentView {
    let v = claiming();
    v.item_commands.set_flag("one", "playtest-answer-command", true);
    v
}

fn answer_playtest(v: &AgentView, args: Value, dir: &Path) -> Value {
    run(&cmd("playtest", args), "one", &mut Seen::default(), v, &|_| {}, dir)
}

#[test]
fn playtest_queues_the_answer_with_notes_and_picture_paths() {
    let v = playtest_view();
    let dir = tempfile::tempdir().unwrap();
    let args = json!({ "id": "a", "result": "fail", "notes": " Broken. ", "pictures": [picture(10)] });
    assert_eq!(answer_playtest(&v, args, dir.path())["type"], "ack");
    let ItemCommand::Playtest { id, passed, notes, .. } = v.item_commands.take("one").remove(0) else {
        panic!("not a playtest")
    };
    assert_eq!((id.as_str(), passed), ("a", false));
    assert!(notes.unwrap().starts_with("Broken.\nPicture: "));
    let pass = answer_playtest(&v, json!({ "id": "a", "result": "pass" }), dir.path());
    assert_eq!(pass["type"], "ack");
    let ItemCommand::Playtest { passed, notes, .. } = v.item_commands.take("one").remove(0) else {
        panic!("not a playtest")
    };
    assert!(passed && notes.is_none());
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
        json!({ "id": "  ", "result": "pass" }),
        json!({ "id": "a" }),
        json!({ "id": "a", "result": "maybe" }),
    ] {
        assert_eq!(answer_playtest(&v, args, &d)["type"], "nack");
    }
    let off = view();
    off.item_commands.set_flag("one", "playtest-answer-command", true);
    let answer = answer_playtest(&off, json!({ "id": "a", "result": "pass" }), &d);
    assert!(answer["reason"].as_str().unwrap().contains("claim mode"));
    let flagless = claiming();
    assert_eq!(
        answer_playtest(&flagless, json!({ "id": "a", "result": "pass" }), &d)["type"],
        "nack"
    );
    assert!(v.item_commands.take("one").is_empty() && off.item_commands.take("one").is_empty());
}

#[test]
fn playtest_with_a_bad_picture_saves_none_and_queues_nothing() {
    let v = playtest_view();
    let dir = tempfile::tempdir().unwrap();
    let args = json!({ "id": "a", "result": "pass", "pictures": [picture(10), { "type": "text/plain", "data": "x" }] });
    assert_eq!(answer_playtest(&v, args, dir.path())["type"], "nack");
    assert!(
        !dir.path().join("attachments").exists()
            || std::fs::read_dir(dir.path().join("attachments")).unwrap().count() == 0
    );
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
    };
    v.live
        .set_items("one", vec![held("a", "building"), held("b", "awaiting_playtest")]);
    let d = data_dir();
    assert_eq!(
        answer_playtest(&v, json!({ "id": "a", "result": "pass" }), &d)["type"],
        "nack"
    );
    assert_eq!(
        answer_playtest(&v, json!({ "id": "b", "result": "pass" }), &d)["type"],
        "ack"
    );
    assert_eq!(
        answer_playtest(&v, json!({ "id": "other", "result": "pass" }), &d)["type"],
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
    for name in ["restart_play", "stop_play"] {
        assert_eq!(
            go(name, json!({ "lane": 0 }), &v)["reason"],
            format!("Not a command this agent takes yet: {name}")
        );
    }
    v.item_commands.set_flag("one", "restart-play-command", true);
    assert_eq!(
        go("restart_play", json!({}), &v)["reason"],
        "restart_play needs a lane."
    );
    assert_eq!(
        go("restart_play", json!({ "lane": 3 }), &v)["reason"],
        "This computer has no lane 3."
    );
    assert_eq!(
        go("restart_play", json!({ "lane": 1 }), &v)["reason"],
        "Not a command this agent takes yet: restart_play"
    );
    assert_eq!(
        go("restart_play", json!({ "lane": 0 }), &v)["reason"],
        "No play command set for this project."
    );
    assert_eq!(go("stop_play", json!({}), &v)["reason"], "stop needs a lane.");
}

#[test]
fn stop_play_stops_play_main_like_stop() {
    let root = tempfile::tempdir().unwrap();
    let id = "cmd-stop-play";
    let mut v = view();
    v.projects[0].id = id.into();
    v.projects[0].repo_path = crate::test_support::repo_with_origin(root.path());
    v.projects[0].worktrees_dir = root.path().join("worktrees");
    v.projects[0].base_branch = "main".into();
    #[cfg(windows)]
    let long = "ping -n 60 127.0.0.1 >nul";
    #[cfg(not(windows))]
    let long = "sleep 60";
    v.projects[0].play_command = long.into();
    v.item_commands.set_flag(id, "restart-play-command", true);
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

mod gallery_read {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn setup(files: &[(&str, Vec<u8>)]) -> (tempfile::TempDir, AgentView) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = paths::gallery_dir(tmp.path(), "one", "shots");
        std::fs::create_dir_all(&dir).unwrap();
        let images: Vec<Value> = files.iter().map(|(f, _)| json!({ "file": f, "title": f })).collect();
        let index = json!({ "name": "Shots", "shotAt": "2026-01-01T00:00:00Z", "sha": "abc", "images": images });
        std::fs::write(dir.join("index.json"), index.to_string()).unwrap();
        for (f, b) in files {
            std::fs::write(dir.join(f), b).unwrap();
        }
        let v = view();
        v.item_commands.set_flag("one", "gallery-read-commands", true);
        (tmp, v)
    }

    fn ask(v: &AgentView, tmp: &Path, name: &str, args: Value) -> Value {
        run(&cmd(name, args), "one", &mut Seen::default(), v, &|_| {}, tmp)
    }

    fn noisy_jpeg() -> Vec<u8> {
        let img = ImageBuffer::from_fn(2400, 1800, |x, y| {
            let n = x.wrapping_mul(2654435761) ^ y.wrapping_mul(40503);
            Rgb([(n >> 3) as u8, (n >> 11) as u8, (n >> 19) as u8])
        });
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 98)
            .encode_image(&img)
            .unwrap();
        out
    }

    #[test]
    fn index_and_image_are_acked() {
        use base64::Engine;
        let bytes = vec![0xff, 0xd8, 1, 2, 3];
        let (tmp, v) = setup(&[("a.jpg", bytes.clone())]);
        let idx = ask(&v, tmp.path(), "gallery_index", json!({ "gallery": "shots" }));
        assert_eq!(idx["type"], "ack", "{idx}");
        assert_eq!(idx["answer"]["name"], "Shots");
        assert_eq!(idx["answer"]["shotAt"], "2026-01-01T00:00:00Z");
        assert_eq!(idx["answer"]["sha"], "abc");
        assert_eq!(idx["answer"]["images"][0]["file"], "a.jpg");
        let img = ask(
            &v,
            tmp.path(),
            "gallery_image",
            json!({ "gallery": "shots", "file": "a.jpg" }),
        );
        assert_eq!(img["type"], "ack", "{img}");
        assert_eq!(img["answer"]["file"], "a.jpg");
        assert_eq!(img["answer"]["type"], "image/jpeg");
        let data = img["answer"]["data"].as_str().unwrap();
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(data).unwrap(), bytes);
    }

    #[test]
    fn bad_requests_are_nacked() {
        let (tmp, v) = setup(&[("a.jpg", vec![1])]);
        for (name, args) in [
            ("gallery_index", json!({ "gallery": "nope" })),
            ("gallery_index", json!({ "gallery": "../x" })),
            ("gallery_index", json!({})),
            ("gallery_image", json!({ "gallery": "nope", "file": "a.jpg" })),
            ("gallery_image", json!({ "gallery": "../x", "file": "a.jpg" })),
            ("gallery_image", json!({ "gallery": "shots", "file": "b.jpg" })),
            ("gallery_image", json!({ "gallery": "shots", "file": "../a.jpg" })),
            ("gallery_image", json!({ "gallery": "shots", "file": "x/a.jpg" })),
            ("gallery_image", json!({ "gallery": "shots" })),
        ] {
            let answer = ask(&v, tmp.path(), name, args.clone());
            assert_eq!(answer["type"], "nack", "{name} {args}: {answer}");
        }
    }

    #[test]
    fn an_off_flag_leaves_the_commands_unported() {
        let (tmp, v) = setup(&[]);
        v.item_commands.set_flag("one", "gallery-read-commands", false);
        let answer = ask(&v, tmp.path(), "gallery_index", json!({ "gallery": "shots" }));
        assert!(answer["reason"].as_str().unwrap().starts_with("Not a command"));
    }

    #[test]
    fn a_big_picture_is_shrunk_and_the_log_holds_no_base64() {
        let bytes = noisy_jpeg();
        assert!(bytes.len().div_ceil(3) * 4 > galleries::MAX_PICTURE_BASE64);
        let (tmp, v) = setup(&[("big.jpg", bytes.clone())]);
        let c = cmd("gallery_image", json!({ "gallery": "shots", "file": "big.jpg" }));
        let answer = run(&c, "one", &mut Seen::default(), &v, &|_| {}, tmp.path());
        assert_eq!(answer["type"], "ack", "{answer}");
        assert!(answer["answer"]["data"].as_str().unwrap().len() <= galleries::MAX_PICTURE_BASE64);
        assert!(answer.to_string().len() < MAX_MESSAGE_BYTES);
        let on_disk = paths::gallery_dir(tmp.path(), "one", "shots").join("big.jpg");
        assert_eq!(std::fs::read(on_disk).unwrap(), bytes);

        audit(tmp.path(), &c, &answer, "wss://x");
        let log = std::fs::read_to_string(tmp.path().join("remote.log")).unwrap();
        assert!(log.contains("\"answerBytes\":"));
        assert!(!log.contains(answer["answer"]["data"].as_str().unwrap()));
        assert!(log.len() < 1000);
    }

    #[test]
    fn a_replayed_or_expired_read_is_refused() {
        let (tmp, v) = setup(&[("a.jpg", vec![1])]);
        let c = cmd("gallery_image", json!({ "gallery": "shots", "file": "a.jpg" }));
        let mut seen = Seen::default();
        let go = |c: &Command, seen: &mut Seen| run(c, "one", seen, &v, &|_| {}, tmp.path());
        assert_eq!(go(&c, &mut seen)["type"], "ack");
        assert_eq!(go(&c, &mut seen)["reason"], "Already received: a command runs once.");
        let mut old = cmd("gallery_index", json!({ "gallery": "shots" }));
        old.id = "c2".into();
        old.issued_at = now_ms() - 61_000;
        old.expires_at = old.issued_at + 60_000;
        assert!(go(&old, &mut seen)["reason"].as_str().unwrap().starts_with("Expired"));
    }
}
