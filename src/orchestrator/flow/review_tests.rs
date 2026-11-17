use super::review::step;
use super::test_rig::{Rig, check, reviewed, rig};
use crate::config::Playtests;
use crate::item_lifecycle::ItemState;
use crate::orchestrator::steps::StepDone;
use crate::store::{FlowRecord, TryOut};

const TRY: &str = r#","playtest":{"what":"Press Save.","steps":["Open the page","Press Save"]}"#;

async fn review_with(r: &Rig, flow: FlowRecord) -> StepDone {
    let mut request = r.request(ItemState::Reviewing);
    request.flow = flow;
    step(r.ctx.clone(), request).await
}

#[tokio::test]
async fn an_approval_goes_on_to_merging_with_the_comment_on_the_change_and_the_head_approved() {
    let r = rig(vec![check("ok", "true", true)]);
    let flow = FlowRecord {
        pr_number: Some(7),
        checks_summary: Some("✓ ok".into()),
        ..Default::default()
    };
    let done = review_with(&r, flow).await;
    assert_eq!(done.run.state, ItemState::Merging);
    let flow = done.flow.unwrap();
    assert_eq!(flow.approved_sha.as_deref(), Some(r.head().as_str()));
    assert_eq!(flow.reviewed_sha, flow.approved_sha);
    assert!(done.run.approved_at_head);
    let comment = r.comment();
    assert!(comment.contains("approve") && comment.contains("✓ ok"), "{comment}");
    assert!(
        done.notes.contains(&"Opus review: approved".to_string()),
        "{:?}",
        done.notes
    );
    let prompt = std::fs::read_to_string(r.root.join("reviewer1.txt")).unwrap();
    assert!(
        prompt.contains("A thing") && prompt.contains("(none given)"),
        "{prompt}"
    );
}

#[tokio::test]
async fn requested_changes_go_back_to_the_implementer_and_the_re_review_reads_what_changed_since() {
    let r = rig(vec![]);
    r.script_review(1, reviewed("request_changes", "Needs work.", ""));
    let done = review_with(&r, FlowRecord::default()).await;
    assert_eq!(done.run.state, ItemState::Implementing);
    assert_eq!(done.run.review_rounds, 1);
    let message = done.send_back.unwrap();
    assert!(
        message.starts_with("The reviewer requested changes (round 1):\n\nNeeds work.")
            && message.contains("- rename the thing")
            && message.ends_with("Fix these, commit, and return the result again."),
        "{message}"
    );
    let flow = done.flow.unwrap();
    assert_eq!(flow.approved_sha, None);
    assert_eq!(flow.last_review.as_deref(), Some(message.as_str()));
    // The implementer fixed it: the next round reads only what changed since.
    let first = r.head();
    r.commit("fix.rs", "fn fix() {}");
    let again = review_with(&r, flow).await;
    assert_eq!(again.run.state, ItemState::Merging);
    let prompt = std::fs::read_to_string(r.root.join("reviewer2.txt")).unwrap();
    assert!(
        prompt.contains("re-review") && prompt.contains(&format!("{first} HEAD")),
        "{prompt}"
    );
}

#[tokio::test]
async fn changes_past_max_review_rounds_need_a_person() {
    let r = rig(vec![]);
    r.script_review(1, reviewed("request_changes", "Again.", ""));
    let mut request = r.request(ItemState::Reviewing);
    request.run.review_rounds = r.ctx.project.max_review_rounds;
    let done = step(r.ctx.clone(), request).await;
    assert_eq!(done.run.state, ItemState::NeedsHuman);
    assert_eq!(done.send_back, None);
}

#[tokio::test]
async fn a_head_the_approval_covers_needs_no_review() {
    let r = rig(vec![]);
    let flow = FlowRecord {
        approved_sha: Some(r.head()),
        ..Default::default()
    };
    let done = review_with(&r, flow).await;
    assert_eq!(done.run.state, ItemState::Merging);
    assert_eq!(r.reviews(), 0);
    assert!(
        done.notes[0].starts_with("Already approved at this commit"),
        "{:?}",
        done.notes
    );
}

#[tokio::test]
async fn a_rebase_that_leaves_the_approved_changes_intact_needs_no_review() {
    let r = rig(vec![]);
    let approved = r.head();
    // The head moves with a commit that adds nothing to the slice's own changes (an empty one).
    let out = std::process::Command::new("git")
        .current_dir(&r.lane)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "merge",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let flow = FlowRecord {
        approved_sha: Some(approved),
        ..Default::default()
    };
    let done = review_with(&r, flow).await;
    assert_eq!(done.run.state, ItemState::Merging);
    assert_eq!(r.reviews(), 0);
    assert!(done.notes[0].contains("approved changes intact"), "{:?}", done.notes);
    assert_eq!(done.flow.unwrap().approved_sha, Some(r.head()));
}

#[tokio::test]
async fn a_persons_steps_or_a_decision_stop_it_with_the_reason() {
    let r = rig(vec![]);
    r.script_review(
        1,
        reviewed(
            "person_steps",
            "Right.",
            r#","personSteps":["Set the secret","Deploy"]"#,
        ),
    );
    let done = review_with(&r, FlowRecord::default()).await;
    assert_eq!(done.run.state, ItemState::NeedsHuman);
    assert!(
        done.notes
            .iter()
            .any(|n| n == "Only steps a person can do are left:\n1. Set the secret\n2. Deploy")
    );
    r.script_review(2, reviewed("needs_human", "Pick one.", ""));
    let done = review_with(&r, FlowRecord::default()).await;
    assert_eq!(done.run.state, ItemState::NeedsHuman);
    assert!(done.notes.iter().any(|n| n == "Reviewer: Pick one."));
}

#[tokio::test]
async fn a_try_out_the_reviewer_asks_for_goes_to_a_person_or_waits_for_the_merge_by_the_setting() {
    let mut r = rig(vec![]);
    r.script_review(1, reviewed("approve", "Fine.", TRY));
    r.script_review(2, reviewed("approve", "Fine.", TRY));
    r.ctx.project.playtests = Playtests::On;
    let done = review_with(&r, FlowRecord::default()).await;
    assert_eq!(done.run.state, ItemState::AwaitingPlaytest);
    let want = TryOut {
        what: "Press Save.".into(),
        steps: vec!["Open the page".into(), "Press Save".into()],
    };
    assert_eq!(done.flow.unwrap().playtest, Some(want.clone()));
    r.ctx.project.playtests = Playtests::AfterMerge;
    let done = review_with(&r, FlowRecord::default()).await;
    assert_eq!(done.run.state, ItemState::Merging);
    let flow = done.flow.unwrap();
    assert_eq!((flow.playtest, flow.pending_playtest), (None, Some(want)));
}

#[tokio::test]
async fn a_diff_that_raises_the_tier_holds_the_change_for_a_persons_check_with_the_reasons() {
    let mut r = rig(vec![]);
    r.ctx.project.playtests = Playtests::Off;
    r.commit("package.json", r#"{"dependencies":{"left-pad":"1.0.0"}}"#);
    let done = review_with(&r, FlowRecord::default()).await;
    assert_eq!(done.run.state, ItemState::AwaitingPlaytest);
    let flow = done.flow.unwrap();
    let held = flow.held.expect("held");
    assert!(
        flow.playtest
            .unwrap()
            .what
            .contains(&format!("Held for a check: {held}"))
    );
    assert!(
        done.notes.iter().any(|n| n.contains("raised its tier")),
        "{:?}",
        done.notes
    );
}
