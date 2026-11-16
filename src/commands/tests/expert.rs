use super::*;
use crate::commands::expert::answer_sessions;
use crate::view::{ExpertAction, ExpertCommand};

const PICTURE: &str = "0a1b2c3d-0000-4000-8000-00000000000a.png";

#[test]
fn the_expert_commands_are_queued_for_the_loop_and_answer_pending() {
    let v = view();
    let noop = |_: i32| {};
    let go = |name: &str, args: Value| run_cmd(&cmd(name, args), &v, &noop);
    // Nothing runs outside claim mode.
    assert_eq!(go("expert_start", json!({ "request": "Fix it" }))["type"], "nack");
    v.item_commands.set_claim(true);
    for (name, args) in [
        ("expert_start", json!({ "request": " Fix it " })),
        (
            "expert_say",
            json!({ "itemId": "e", "text": "more", "now": true, "pictureIds": [PICTURE] }),
        ),
        ("expert_stop", json!({ "itemId": "e" })),
        ("expert_end", json!({ "itemId": "e", "outcome": "park" })),
        ("session_history", json!({ "itemId": "e", "before": 500, "limit": 40 })),
    ] {
        assert_eq!(go(name, args)["type"], "pending", "{name}");
    }
    let q = v.item_commands.take_experts("one");
    let actions: Vec<ExpertAction> = q.iter().map(|c| c.action.clone()).collect();
    assert_eq!(
        actions,
        [
            ExpertAction::Start {
                request: "Fix it".into(),
                continue_from: None,
            },
            ExpertAction::Say {
                item: "e".into(),
                text: "more".into(),
                pictures: vec![PICTURE.into()],
                now: true
            },
            ExpertAction::Stop { item: "e".into() },
            ExpertAction::End {
                item: "e".into(),
                outcome: "park".into()
            },
            ExpertAction::History {
                item: "e".into(),
                before: Some(500),
                limit: Some(40)
            },
        ]
    );
    assert!(q.iter().all(|c: &ExpertCommand| c.by == "jc" && c.command_id == "c1"));
}

#[test]
fn bad_expert_commands_are_nacked_and_queue_nothing() {
    let v = view();
    v.item_commands.set_claim(true);
    let noop = |_: i32| {};
    let bad = [
        ("expert_start", json!({ "request": "  " })),
        ("expert_start", json!({ "request": "x".repeat(4001) })),
        (
            "expert_start",
            json!({ "request": "x", "continue": { "sessionId": "s" } }),
        ),
        ("expert_start", json!({ "request": "x", "continue": "../../etc" })),
        ("expert_start", json!({ "request": "x", "continue": "" })),
        ("expert_say", json!({ "text": "hi" })),
        ("expert_say", json!({ "itemId": "e" })),
        (
            "expert_say",
            json!({ "itemId": "e", "text": "hi", "pictureIds": ["../../etc/passwd"] }),
        ),
        ("expert_say", json!({ "itemId": "e", "text": "hi", "pictureIds": "x" })),
        (
            "expert_say",
            json!({ "itemId": "e", "text": "hi", "pictureIds": [PICTURE, PICTURE, PICTURE, PICTURE, PICTURE] }),
        ),
        ("expert_stop", json!({})),
        ("expert_end", json!({ "itemId": "e", "outcome": "burn" })),
        ("expert_end", json!({ "outcome": "park" })),
    ];
    for (name, args) in bad {
        let a = run_cmd(&cmd(name, args.clone()), &v, &noop);
        assert_eq!(a["type"], "nack", "{name} {args}");
    }
    assert!(v.item_commands.take_experts("one").is_empty());
}

#[test]
fn claude_sessions_answers_the_sessions_outside_the_lanes_and_none_of_their_words() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects").join("p");
    std::fs::create_dir_all(&root).unwrap();
    let line = |cwd: &str| {
        json!({ "type": "user", "cwd": cwd, "gitBranch": "feature/a", "message": { "content": "secret plans" } })
            .to_string()
    };
    std::fs::write(root.join("mine.jsonl"), line("/not/a/folder")).unwrap();
    let lanes = tmp.path().join("lanes");
    std::fs::write(root.join("lane.jsonl"), line(&lanes.join("lane-1").to_string_lossy())).unwrap();
    let a = answer_sessions("c1", &tmp.path().join("projects"), &[lanes]);
    assert_eq!((a["type"].as_str(), a["id"].as_str()), (Some("ack"), Some("c1")));
    let sessions = a["answer"]["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1, "{a}");
    assert_eq!(sessions[0]["id"], "mine");
    assert_eq!(sessions[0]["branch"], "feature/a");
    // The title is the person's first words; the list holds no message of the conversation as such.
    assert!(
        sessions[0]
            .as_object()
            .unwrap()
            .keys()
            .all(|k| ["id", "title", "folder", "branch", "at"].contains(&k.as_str()))
    );
}
