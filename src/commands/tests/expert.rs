use super::*;
use crate::view::{ExpertAction, ExpertCommand};

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
            json!({ "itemId": "e", "text": "more", "now": true, "pictureIds": ["p1"] }),
        ),
        ("expert_stop", json!({ "itemId": "e" })),
        ("expert_end", json!({ "itemId": "e", "outcome": "park" })),
    ] {
        assert_eq!(go(name, args)["type"], "pending", "{name}");
    }
    let q = v.item_commands.take_experts("one");
    let actions: Vec<ExpertAction> = q.iter().map(|c| c.action.clone()).collect();
    assert_eq!(
        actions,
        [
            ExpertAction::Start {
                request: "Fix it".into()
            },
            ExpertAction::Say {
                item: "e".into(),
                text: "more".into(),
                pictures: vec!["p1".into()],
                now: true
            },
            ExpertAction::Stop { item: "e".into() },
            ExpertAction::End {
                item: "e".into(),
                outcome: "park".into()
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
        ("expert_say", json!({ "text": "hi" })),
        ("expert_say", json!({ "itemId": "e" })),
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
