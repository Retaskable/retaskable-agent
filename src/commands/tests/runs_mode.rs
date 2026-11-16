//! In `runs` mode the commands that aren't about an item, and lane play, stop and restart, are taken; the item commands are not.
use super::*;

fn runs_view() -> AgentView {
    let mut v = view();
    v.projects[0].play_command = "game".into();
    v.item_commands.set_runs(true);
    v
}

fn go(v: &AgentView, name: &str, args: Value) -> Value {
    run_cmd(&cmd(name, args), v, &|_: i32| {})
}

#[test]
fn experts_ideas_and_drafts_are_queued_in_runs_mode() {
    let v = runs_view();
    assert_eq!(
        go(&v, "expert_start", json!({ "request": "Fix it" }))["type"],
        "pending"
    );
    assert_eq!(go(&v, "idea_accept", json!({ "idea": "i1" }))["type"], "pending");
    for name in ["draft_approve", "draft_discard", "merge_pr"] {
        assert_eq!(go(&v, name, json!({ "draftId": "d1" }))["type"], "ack", "{name}");
    }
    assert_eq!(v.item_commands.take_experts("one").len(), 1);
    assert_eq!(v.item_commands.take_ideas("one").len(), 1);
    assert_eq!(v.item_commands.take_drafts("one").len(), 3);
}

#[test]
fn item_commands_are_refused_in_runs_mode() {
    let v = runs_view();
    let refused = |answer: Value| {
        assert_eq!(answer["type"], "nack", "{answer}");
        assert!(
            answer["reason"].as_str().unwrap().contains("isn't running work"),
            "{answer}"
        );
    };
    refused(go(&v, "continue", json!({ "itemId": "a" })));
    refused(go(&v, "retry", json!({ "itemId": "a" })));
    refused(go(&v, "skip", json!({ "itemId": "a" })));
    assert!(v.item_commands.take_lane("one").is_empty());
}

#[test]
fn lane_play_stop_and_restart_are_queued_in_runs_mode() {
    let v = runs_view();
    for name in ["play", "stop", "stop_play", "restart_play"] {
        assert_eq!(go(&v, name, json!({ "lane": 1 }))["type"], "ack", "{name}");
    }
    let queued = v.item_commands.take_lane("one");
    assert!(matches!(
        queued.as_slice(),
        [
            ItemCommand::Play { lane: 1, .. },
            ItemCommand::Stop { lane: 1, .. },
            ItemCommand::Stop { lane: 1, .. },
            ItemCommand::RestartPlay { lane: 1, .. },
        ]
    ));
}

#[test]
fn lane_play_is_checked_in_runs_mode_as_in_claim_mode() {
    let mut v = runs_view();
    v.projects[0].play_command = " ".into();
    for name in ["play", "restart_play"] {
        let answer = go(&v, name, json!({ "lane": 1 }));
        assert_eq!(answer["reason"], "No play command set for this project.", "{name}");
    }
    v.projects[0].play_command = "game".into();
    let answer = go(&v, "play", json!({ "lane": 99 }));
    assert_eq!(answer["reason"], "This computer has no lane 99.");
    assert!(v.item_commands.take_lane("one").is_empty());
}

#[test]
fn lane_play_is_refused_when_nothing_runs_work() {
    let v = view();
    let answer = go(&v, "play", json!({ "lane": 1 }));
    assert_eq!(answer["type"], "nack");
    assert!(answer["reason"].as_str().unwrap().contains("isn't running work"));
}

#[test]
fn nothing_is_taken_outside_claim_and_runs_modes() {
    let v = view();
    assert_eq!(go(&v, "expert_start", json!({ "request": "Fix it" }))["type"], "nack");
    assert_eq!(go(&v, "idea_accept", json!({ "idea": "i1" }))["type"], "nack");
    assert_eq!(go(&v, "draft_approve", json!({ "draftId": "d1" }))["type"], "nack");
}
