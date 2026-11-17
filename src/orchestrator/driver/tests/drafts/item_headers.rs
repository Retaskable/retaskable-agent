//! A plan never goes ready naming an item no Items header adds (`driver/plan_items.rs`): the drafter is asked once to add it, and
//! a draft whose drafter still hasn't fails with that reason (draft-28e6e812, 2026-11-17).
use super::*;
use super::{reports, rig_with};

fn specs(items: &str) -> Vec<(String, String)> {
    let text = format!("# Docker CI\n\nTheme: Orchestration\n\nKind: change\n\n{items}\n## 1. Body\n");
    vec![("docs/specs/agent-ci.md".to_string(), text)]
}

const HEADER: &str = "Items:\n- **docker-ci-info-line**: Docker CI line is informational. Depends on: none.\n";

fn answer() -> Value {
    json!({"status": "pr_opened", "title": "Docker CI", "changeTitle": "Spec: Docker CI", "summary": "Informational",
        "itemIds": ["docker-ci-info-line"], "pieces": [{"title": "Docker CI", "kind": "tweak"}]})
}

fn claimed(extra: Value) -> Value {
    let mut d = json!({ "id": "d1", "request": "Make the Docker CI line informational", "titles": ["Docker CI"] });
    d.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    d
}

#[tokio::test]
async fn a_plan_with_the_header_is_reported_ready_with_no_correction() {
    let (mut rig, host, asked) = rig_with(claimed(json!({})), vec![answer()]).await;
    *host.specs.lock().unwrap() = vec![specs(HEADER)];
    let sent = reports(&mut rig, &asked, 2).await;
    assert_eq!(sent[1]["detail"], "awaiting_approval");
    assert_eq!(sent[1]["itemIds"], json!(["docker-ci-info-line"]));
    assert_eq!(host.jobs.lock().unwrap().len(), 1, "the drafter ran once");
}

#[tokio::test]
async fn a_plan_naming_an_item_no_header_adds_is_asked_to_add_it_and_reported_once_it_has() {
    let (mut rig, host, asked) = rig_with(claimed(json!({})), vec![answer(), answer()]).await;
    // First read: no header. Second read, after the correction: the drafter added it.
    *host.specs.lock().unwrap() = vec![specs(""), specs(HEADER)];
    let sent = reports(&mut rig, &asked, 2).await;
    assert_eq!(
        sent[1]["detail"], "awaiting_approval",
        "ready only after the header is there"
    );
    assert_eq!(sent[1]["itemIds"], json!(["docker-ci-info-line"]));
    let jobs = host.jobs.lock().unwrap().clone();
    assert_eq!(jobs.len(), 2);
    let (session, resumed, follow_up) = &jobs[1];
    assert!(*resumed && *session == jobs[0].0, "the same session, resumed");
    assert!(
        follow_up.contains("`docker-ci-info-line`")
            && follow_up.contains("- **<id>**: <the piece's title>. Depends on:")
    );
    assert_eq!(
        host.published.lock().unwrap().len(),
        1,
        "one pull request, opened after the check"
    );
}

#[tokio::test]
async fn a_plan_still_missing_its_header_after_the_correction_fails_with_that_reason_and_opens_no_pull_request() {
    let (mut rig, host, asked) = rig_with(claimed(json!({"retried": true})), vec![answer(), answer()]).await;
    *host.specs.lock().unwrap() = vec![specs("")];
    let sent = reports(&mut rig, &asked, 2).await;
    assert_eq!(
        (sent[1]["status"].as_str(), sent[1]["detail"].as_str()),
        (Some("needs_person"), Some("failed"))
    );
    let why = sent[1]["summary"].as_str().unwrap();
    assert!(
        why.contains("docker-ci-info-line") && why.contains("no Items header"),
        "{why}"
    );
    assert_eq!(host.jobs.lock().unwrap().len(), 2, "asked once, not again");
    assert!(host.published.lock().unwrap().is_empty());
    assert!(sent.iter().all(|r| r["detail"] != "awaiting_approval"));
}

#[tokio::test]
async fn a_plan_naming_no_item_needs_no_header() {
    let (mut rig, host, asked) = rig_with(
        claimed(json!({})),
        vec![json!({"status": "pr_opened", "changeTitle": "Spec: Docker CI", "summary": "Written up", "pieces": [{"title": "Docker CI", "kind": "tweak"}]})],
    )
    .await;
    *host.specs.lock().unwrap() = vec![specs("")];
    let sent = reports(&mut rig, &asked, 2).await;
    assert_eq!(sent[1]["detail"], "awaiting_approval");
    assert_eq!(host.jobs.lock().unwrap().len(), 1);
}
