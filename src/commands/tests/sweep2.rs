//! The commands of the second command sweep that are checked and queued here (`answer`, `unlink_project`, the reads and the
//! item and lane commands the hub still pushes).
use super::*;

fn go(v: &AgentView, name: &str, args: Value) -> Value {
    run(&cmd(name, args), "one", &mut Seen::default(), v, &|_| {}, &data_dir())
}

#[test]
fn answer_queues_the_answers_in_order_and_needs_the_item_its_answers_and_claim_mode() {
    let v = claiming();
    assert_eq!(
        go(&v, "answer", json!({ "itemId": " a ", "answers": ["Blue.", ""] })),
        json!({ "type": "ack", "id": "c1" })
    );
    let ItemCommand::Answer {
        item,
        answers,
        attached,
        ..
    } = v.item_commands.take("one").remove(0)
    else {
        panic!("not an answer")
    };
    assert_eq!(
        (item.as_str(), answers),
        ("a", vec!["Blue.".to_string(), String::new()])
    );
    assert!(attached.is_none());
    for args in [
        json!({ "answers": ["x"] }),
        json!({ "itemId": "a" }),
        json!({ "itemId": "a", "answers": [1] }),
    ] {
        assert_eq!(go(&v, "answer", args)["type"], "nack");
    }
    assert!(v.item_commands.take("one").is_empty());
    let off = view();
    let nack = go(&off, "answer", json!({ "itemId": "a", "answers": ["x"] }));
    assert_eq!(
        nack["reason"],
        "This computer isn't running work (the orchestrator isn't in claim mode)."
    );
}

#[test]
fn answer_with_pictures_goes_up_pending_for_the_loop_to_fetch_them() {
    let v = claiming();
    let args = json!({ "itemId": "a", "answers": ["See"], "pictureIds": [picture_id('b')] });
    assert_eq!(go(&v, "answer", args), json!({ "type": "pending", "id": "c1" }));
    let ItemCommand::Answer { attached, .. } = v.item_commands.take("one").remove(0) else {
        panic!("not an answer")
    };
    assert_eq!(attached.unwrap().ids, vec![picture_id('b')]);
}
