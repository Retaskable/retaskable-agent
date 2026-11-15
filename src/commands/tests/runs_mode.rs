//! In `runs` mode the commands that aren't about an item are taken, and the item commands and lane play are not.
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
fn item_commands_and_lane_play_are_refused_in_runs_mode() {
    let v = runs_view();
    let refused = |answer: Value| {
        assert_eq!(answer["type"], "nack", "{answer}");
        assert!(
            answer["reason"].as_str().unwrap().contains("isn't running work"),
            "{answer}"
        );
    };
    refused(go(&v, "play", json!({ "lane": 1 })));
    refused(go(&v, "stop", json!({ "lane": 1 })));
    refused(go(&v, "restart_play", json!({ "lane": 1 })));
    refused(go(&v, "continue", json!({ "id": "a" })));
    refused(go(&v, "retry", json!({ "id": "a" })));
    refused(go(&v, "skip", json!({ "id": "a" })));
    assert!(v.item_commands.take_lane("one").is_empty());
}

#[test]
fn nothing_is_taken_outside_claim_and_runs_modes() {
    let v = view();
    assert_eq!(go(&v, "expert_start", json!({ "request": "Fix it" }))["type"], "nack");
    assert_eq!(go(&v, "idea_accept", json!({ "idea": "i1" }))["type"], "nack");
    assert_eq!(go(&v, "draft_approve", json!({ "draftId": "d1" }))["type"], "nack");
}
