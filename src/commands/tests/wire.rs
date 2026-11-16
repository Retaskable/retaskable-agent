//! The arguments the hub sends, as `shared/cloudCommands.ts` spells them (the schemas are strict, so a name the hub
//! doesn't use never arrives): `itemId` for an item, `draftId` for a draft, `pictureIds` for pictures, and no lane
//! for Play main.
use super::*;

fn send(v: &AgentView, name: &str, args: Value) -> Value {
    run(&cmd(name, args), "one", &mut Seen::default(), v, &|_| {}, &data_dir())
}

#[test]
fn continue_retry_and_skip_take_the_hubs_item_id() {
    let v = claiming();
    let png = picture_id('c');
    assert_eq!(
        send(&v, "continue", json!({ "itemId": "7", "notes": "Go on." }))["type"],
        "ack"
    );
    assert_eq!(send(&v, "retry", json!({ "itemId": "7" }))["type"], "ack");
    assert_eq!(send(&v, "skip", json!({ "itemId": "7" }))["type"], "ack");
    assert_eq!(
        send(
            &v,
            "continue",
            json!({ "itemId": "7", "notes": "", "pictureIds": [png] })
        ),
        json!({ "type": "pending", "id": "c1" })
    );
    let queued = v.item_commands.take("one");
    assert!(matches!(
        queued.as_slice(),
        [
            ItemCommand::Continue { item: a, notes: Some(n), attached: None, .. },
            ItemCommand::StartOver { item: b, notes: None, .. },
            ItemCommand::Skip { item: c, .. },
            ItemCommand::Continue { item: d, notes: None, attached: Some(at), .. },
        ] if [a, b, c, d].iter().all(|i| *i == "7") && n == "Go on." && at.ids == [png.clone()]
    ));
}

#[test]
fn draft_files_takes_the_hubs_draft_id() {
    let v = view();
    let missing = send(&v, "draft_files", json!({ "draftId": "d-1" }));
    assert_eq!(
        missing["reason"],
        "No worktree for draft d-1 on this computer (it may be finished or removed)."
    );
    assert_eq!(
        send(&v, "draft_files", json!({ "id": "d-1" }))["reason"],
        "draft_files needs the draft's id."
    );
}

#[test]
fn play_and_stop_play_with_no_args_are_play_main_and_restart_play_names_what_to_restart() {
    let v = view();
    // Play main on a project with no play command: the same words as `{ lane: 0 }`.
    let main = send(&v, "play", json!({ "lane": 0 }));
    assert_eq!(send(&v, "play", json!({})), main);
    assert_eq!(main["reason"], "No play command set for this project.");
    assert_eq!(send(&v, "stop_play", json!({}))["reason"], "Play main isn't running.");
    assert_eq!(
        send(&v, "stop_play", json!({}))["reason"],
        send(&v, "stop_play", json!({ "lane": 0 }))["reason"]
    );
    assert_eq!(send(&v, "restart_play", json!({}))["reason"], "Name an item or a lane.");
    for name in ["play", "stop_play", "restart_play"] {
        assert_eq!(
            send(&v, name, json!({ "itemId": "7", "lane": 1 }))["reason"],
            "Name an item or a lane, not both.",
            "{name}"
        );
    }
}

#[test]
fn reads_the_hub_pushes_are_queued_for_the_loop_in_claim_and_runs_mode() {
    use crate::view::ReadCommand;
    // `read_file { path }`, `issues_preview { label?, milestone?, anyone? }` and `environments {}` (`shared/cloudCommands.ts`).
    let reads = [
        ("read_file", json!({ "path": "docs/specs/agent.md" })),
        ("issues_preview", json!({ "label": "bug", "anyone": true })),
        ("environments", json!({})),
    ];
    for v in [claiming(), {
        let v = view();
        v.item_commands.set_runs(true);
        v
    }] {
        for (name, args) in &reads {
            assert_eq!(
                send(&v, name, args.clone()),
                json!({ "type": "pending", "id": "c1" }),
                "{name}"
            );
        }
        let queued = v.item_commands.take_reads("one");
        assert_eq!(queued.len(), 3);
        assert_eq!(
            queued[0],
            ReadCommand {
                project: "one".into(),
                command_id: "c1".into(),
                kind: "read_file".into(),
                args: json!({ "path": "docs/specs/agent.md" })
            }
        );
    }
    let idle = view();
    let refused = send(&idle, "read_file", json!({ "path": "a.md" }));
    assert_eq!(refused["type"], "nack");
    assert!(idle.item_commands.take_reads("one").is_empty());
}
