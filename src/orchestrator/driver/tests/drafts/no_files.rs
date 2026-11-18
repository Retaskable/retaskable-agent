//! A plan that changed no file (`driver/drafts/no_files.rs`; the TS agent's `noFiles` in `drafts.ts`): its items go straight to the
//! backlog and the draft finishes `merged`, with no pull request and nothing for the store to approve.
use super::super::asks::ops_named;
use super::*;

fn plan(items: Value) -> Value {
    json!({ "status": "pr_opened", "title": "Charts", "changeTitle": "Spec: Charts", "summary": "Links to the specs", "itemIds": ["not-this"],
        "items": items,
        "breakdown": { "build": [{ "id": "chart-view", "line": "Draw them", "size": "M" }], "people": [] } })
}

fn items() -> Value {
    json!([
        { "id": "chart-data", "title": "Chart data", "dependsOn": [], "specRefs": ["docs/specs/charts.md section 2"], "kind": "feature", "checking": "auto" },
        { "id": "chart-view", "title": "Chart view", "dependsOn": ["chart-data", "elsewhere"], "specRefs": ["docs/specs/charts.md"] }
    ])
}

async fn rig_no_files(answer: Value) -> (Rig, Arc<Host>, Asked) {
    let (rig, host, asked) = rig_with(
        json!({ "id": "d1", "request": "Add charts", "requestedBy": "ada" }),
        vec![answer],
    )
    .await;
    host.no_files.store(true, Ordering::SeqCst);
    (rig, host, asked)
}

fn added() -> Value {
    json!({ "via": "draft", "items": [
        { "id": "chart-data", "title": "Chart data", "dependsOn": [], "specRefs": ["docs/specs/charts.md"],
          "kind": "feature", "checking": "auto", "requestedBy": "ada" },
        { "id": "chart-view", "title": "Chart view", "dependsOn": ["chart-data"], "specRefs": ["docs/specs/charts.md"],
          "checking": "manual", "requestedBy": "ada" }
    ] })
}

#[tokio::test]
async fn a_plan_that_changed_no_file_adds_its_items_and_finishes_merged_with_no_pull_request() {
    let (mut rig, host, asked) = rig_no_files(plan(items())).await;
    let sent = reports(&mut rig, &asked, 2).await;
    for _ in 0..30 {
        if !ops_named(&asked, "add").is_empty() {
            break;
        }
        rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // Nothing published, nothing for the store to approve: one report, `merged`, with the items' ids as the entries.
    assert!(host.published.lock().unwrap().is_empty());
    let done = &sent[1];
    assert_eq!(
        (
            done["status"].as_str(),
            done["detail"].as_str(),
            done["summary"].as_str()
        ),
        (Some("merged"), Some("merged"), Some("Links to the specs"))
    );
    assert_eq!(done["itemIds"], json!(["chart-data", "chart-view"]));
    assert_eq!(done["titles"], json!(["Charts"]));
    assert_eq!(done["breakdown"]["build"][0]["id"], "chart-view");
    assert!(done.get("prUrl").is_none() && done.get("plan").is_none());
    assert!(sent.iter().all(|r| r["detail"] != "awaiting_approval"));
    // The items reach the store as a merged plan's do, with who asked.
    assert_eq!(ops_named(&asked, "add"), [added()]);
}

#[tokio::test]
async fn a_plan_that_changed_a_file_is_published_as_usual() {
    let (mut rig, host, asked) = rig_with(json!({ "id": "d1", "request": "Add charts" }), vec![plan(items())]).await;
    reports(&mut rig, &asked, 2).await;
    assert_eq!(host.published.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_plan_that_changed_no_file_and_planned_no_item_fails_with_nothing_to_approve() {
    let (mut rig, host, asked) = rig_no_files(plan(json!([]))).await;
    host.publish_fails.store(true, Ordering::SeqCst);
    let sent = reports(&mut rig, &asked, 2).await;
    assert_eq!(sent[1]["status"], "queued");
    let said = sent[1]["summary"].as_str().unwrap();
    assert!(
        said.ends_with("The drafter changed no file and planned no item, so there is nothing to approve."),
        "{said}"
    );
    assert!(ops_named(&asked, "add").is_empty());
}
