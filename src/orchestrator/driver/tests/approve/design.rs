//! Skip the design and Try again on a plan waiting for its design review (`driver/design_press.rs`): each press runs
//! in a task of its own and answers its command, `ack` or `nack` with the reason.
use super::*;

/// Queues `press` on a live project and passes until the loop has answered the command.
async fn pressed(intake: Value, press: DraftCommand, paused: bool) -> (Value, Asked, tempfile::TempDir) {
    let (base, asked) = store(intake).await;
    let root = tempfile::tempdir().unwrap();
    let repo = spec_repo(root.path());
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.pause("p0", paused);
    rig.lp(0).project.repo_path = repo.to_string_lossy().into_owned();
    rig.lp(0).file_project.repo_path = repo.to_string_lossy().into_owned();
    rig.lp(0).project.base_branch = "main".into();
    rig.lp(0).file_project.base_branch = "main".into();
    rig.machine.drafts = Some(Arc::new(Host::default()));
    let items = rig.machine.links.item_commands.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    items.set_out("p0", Some(tx));
    items.push_draft(press);
    for _ in 0..500 {
        rig.machine.pass().await;
        if let Ok(answer) = rx.try_recv() {
            return (answer, asked, root);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the press was never answered: {:?}", ops(&asked));
}

fn skip() -> DraftCommand {
    DraftCommand::SkipDesign {
        project: "p0".into(),
        draft: "d1".into(),
        command_id: "c9".into(),
    }
}

#[tokio::test]
async fn skip_the_design_is_the_stores_op_with_no_lease_and_acks_the_command() {
    let (answer, asked, _root) = pressed(json!({ "needsDesign": true }), skip(), false).await;
    assert_eq!(answer, json!({ "type": "ack", "id": "c9" }));
    let skips: Vec<_> = asked
        .lock()
        .unwrap()
        .iter()
        .filter(|(op, _)| op == "draft_skip_design")
        .map(|(_, a)| a.clone())
        .collect();
    assert_eq!(skips, [json!({ "id": "d1" })]);
    assert!(
        !ops(&asked).contains(&"draft_take".to_string()),
        "no lease taken: {:?}",
        ops(&asked)
    );
}

#[tokio::test]
async fn skip_the_design_is_done_while_paused_and_nothing_else_of_the_plan_is() {
    let (answer, asked, _root) = pressed(json!({ "needsDesign": true }), skip(), true).await;
    assert_eq!(answer["type"], "ack");
    assert!(reported(&asked).is_empty() && upserts(&asked).is_empty());
}

#[tokio::test]
async fn a_drawing_that_cannot_be_done_nacks_with_why_and_keeps_why_on_the_plan() {
    let (answer, asked, _root) = pressed(
        json!({ "needsDesign": true }),
        DraftCommand::DrawDesign {
            project: "p0".into(),
            draft: "d1".into(),
            note: Some("Make it a menu".into()),
            command_id: "c9".into(),
        },
        true,
    )
    .await;
    assert_eq!(
        (answer["type"].as_str(), answer["id"].as_str()),
        (Some("nack"), Some("c9"))
    );
    let why = answer["reason"].as_str().unwrap().to_string();
    assert!(!why.is_empty());
    // The plan keeps the reason for its card, and the lease taken for the press is let go of.
    let kept: Vec<_> = reported(&asked)
        .into_iter()
        .filter_map(|r| r["designNote"].as_str().map(str::to_string))
        .collect();
    assert_eq!(kept.len(), 1, "{:?}", ops(&asked));
    assert!(kept[0].starts_with(&why[..why.len().min(20)]));
    assert!(ops(&asked).contains(&"draft_release".to_string()));
}

#[tokio::test]
async fn a_drawing_for_a_plan_with_no_review_is_refused_before_anything_is_drawn() {
    let (answer, asked, _root) = pressed(
        json!({}),
        DraftCommand::DrawDesign {
            project: "p0".into(),
            draft: "d1".into(),
            note: None,
            command_id: "c9".into(),
        },
        false,
    )
    .await;
    assert_eq!(
        answer,
        json!({ "type": "nack", "id": "c9", "reason": "This plan has no design to review." })
    );
    assert!(reported(&asked).is_empty() && ops(&asked).contains(&"draft_release".to_string()));
}
