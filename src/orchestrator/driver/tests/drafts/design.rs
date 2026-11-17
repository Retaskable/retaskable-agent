//! A plan whose change people will see waits for a design review (`driver/drafts.rs`, `design_board`): the report that
//! makes it ready carries the plan's facts, the store answers `needsDesign`, its designs are drawn on the idea's board, and why they weren't is kept.
use super::*;
use super::{Host, reports, rig_with};
use crate::drafts::files::DraftFile;

fn file(path: &str, content: &str) -> DraftFile {
    DraftFile {
        path: path.into(),
        old_path: None,
        is_new: false,
        content: content.into(),
        patch: None,
        deleted: false,
        note: None,
    }
}

const SKETCH: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 390 300"><rect width="390" height="300" fill="#eee"/><text x="20" y="40">Snooze</text></svg>"##;

/// The files of a plan whose spec is a `kind` (`new` is a feature, `change` a tweak) with a sketch.
fn plan_files(kind: &str, sketch: bool) -> Vec<DraftFile> {
    let spec = format!("# Snooze\n\nKind: {kind}\n\nItems:\n- **snooze-1**: Snooze an alert. Depends on: none.\n");
    let mut files = vec![file("docs/specs/snooze.md", &spec)];
    if sketch {
        files.push(file("docs/specs/snooze.preview.svg", SKETCH));
    }
    files
}

fn answer() -> Value {
    json!({"status": "pr_opened", "title": "Snooze", "changeTitle": "Spec: Snooze", "itemIds": ["snooze-1"],
        "summary": "A snooze button", "pieces": [{"title": "Snooze", "kind": "feature"}]})
}

fn layout() -> Value {
    json!({"page": "board", "region": "header",
        "variations": [{"name": "A: button", "pieces": [{"element": "button", "label": "Snooze"}]}]})
}

fn claimed() -> Value {
    json!({ "id": "d1", "request": "Let people snooze", "titles": ["Snooze"] })
}

async fn written(host: &Host, at_least: usize, rig: &mut Rig) {
    for _ in 0..200 {
        rig.machine.pass().await;
        if host.board.sent.lock().unwrap().len() >= at_least {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the designs were never written");
}

#[tokio::test]
async fn a_feature_with_a_picture_reports_its_facts_and_its_designs_are_drawn_because_the_store_said_so() {
    let (mut rig, host, asked) = rig_with(claimed(), vec![answer()]).await;
    *host.files.lock().unwrap() = plan_files("new", true);
    *host.layout.lock().unwrap() = Some(Ok(layout()));
    let sent = reports(&mut rig, &asked, 2).await;
    assert_eq!(sent[1]["detail"], "awaiting_approval");
    // The agent marks nothing itself: it reports the facts and the store answers `needsDesign`.
    assert!(sent[1].get("needsDesign").is_none() && sent[0].get("plan").is_none());
    let plan = &sent[1]["plan"];
    assert_eq!(
        (
            &plan["first"],
            &plan["preview"],
            &plan["itemKinds"],
            &plan["risk"],
            &plan["table"],
            &plan["build"]
        ),
        (
            &json!(true),
            &json!(true),
            &json!(["feature"]),
            &json!(false),
            &json!(false),
            &json!(0)
        )
    );
    assert!(plan["tier"].is_null() && plan["title"].is_string(), "{plan}");
    written(&host, 4, &mut rig).await;
    let (url, _) = host.board.opened.lock().unwrap()[0].clone();
    assert!(url.contains("/board?ticket=t1&board=idea-d1"), "{url}");
    assert!(host.prompts.lock().unwrap()[0].contains("Snooze\n\nA snooze button"));
    assert!(host.board.sent.lock().unwrap().iter().all(|m| m["op"]["kind"] == "add"));
    // Drawn: nothing more is said about why not.
    let said = ops(&asked).iter().filter(|o| *o == "draft_report").count();
    assert_eq!(said, 2);
}

/// The reviews written into the plan's details, as (lease, parsed review).
fn kept(asked: &Asked) -> Vec<(String, Value)> {
    (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == "draft_details_report")
        .map(|(_, a)| {
            let review = a["patch"]["designReview"].as_str().unwrap();
            (
                a["token"].as_str().unwrap().to_string(),
                serde_json::from_str(review).unwrap(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_review_goes_into_the_plans_details_with_the_lease_the_drafter_holds_it_by() {
    let (mut rig, host, asked) = rig_with(claimed(), vec![answer()]).await;
    *host.files.lock().unwrap() = plan_files("new", true);
    let mut drawn = layout();
    drawn["sentence"] = json!("You'll see a snooze button on each alert, so you can pause it.");
    *host.layout.lock().unwrap() = Some(Ok(drawn));
    reports(&mut rig, &asked, 2).await;
    written(&host, 4, &mut rig).await;
    let reviews = kept(&asked);
    assert_eq!(reviews.len(), 1, "{:?}", ops(&asked));
    assert_eq!(reviews[0].0, "lease-d");
    assert_eq!(
        reviews[0].1["sentence"],
        "You'll see a snooze button on each alert, so you can pause it."
    );
    assert_eq!(reviews[0].1["variations"][0]["name"], "A: button");
    assert!(host.rewrites.lock().unwrap().is_empty());
    // The review is in the details, not on the plan's report: nothing about it is sent there.
    let said: Vec<Value> = (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == "draft_report")
        .map(|(_, a)| a.clone())
        .collect();
    assert!(said.iter().all(|r| r.get("designReview").is_none()));
}

#[tokio::test]
async fn a_layout_with_no_sentence_asks_for_one_and_a_failed_ask_keeps_the_safe_sentence() {
    let (mut rig, host, asked) = rig_with(claimed(), vec![answer()]).await;
    *host.files.lock().unwrap() = plan_files("new", true);
    *host.layout.lock().unwrap() = Some(Ok(layout()));
    *host.rewrite.lock().unwrap() = Some(Err("Claude is at its limit".into()));
    reports(&mut rig, &asked, 2).await;
    written(&host, 4, &mut rig).await;
    assert_eq!(host.rewrites.lock().unwrap().len(), 1);
    assert!(host.rewrites.lock().unwrap()[0].contains("The earlier sentence: (none)"));
    assert_eq!(
        kept(&asked)[0].1["sentence"],
        crate::design_board::sentence::SAFE_SENTENCE
    );
}

#[tokio::test]
async fn designs_that_cant_be_drawn_leave_the_plan_waiting_for_its_review_with_the_reason() {
    let (mut rig, host, asked) = rig_with(claimed(), vec![answer()]).await;
    *host.files.lock().unwrap() = plan_files("new", true);
    *host.layout.lock().unwrap() = Some(Err("Claude is at its limit".into()));
    let sent = reports(&mut rig, &asked, 3).await;
    assert!(sent[1].get("needsDesign").is_none() && sent[1]["plan"]["preview"] == true);
    assert_eq!(
        (sent[2]["designNote"].as_str(), sent[2]["token"].as_str()),
        (Some("Claude is at its limit"), Some("lease-d"))
    );
    assert!(host.board.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_tweak_a_plan_with_no_picture_and_a_plan_already_written_need_no_review() {
    // A tweak with a picture.
    let (mut rig, host, asked) = rig_with(claimed(), vec![answer()]).await;
    *host.files.lock().unwrap() = plan_files("change", true);
    let sent = reports(&mut rig, &asked, 2).await;
    assert!(sent[1].get("needsDesign").is_none(), "{:?}", sent[1]);
    assert!(host.prompts.lock().unwrap().is_empty() && host.board.opened.lock().unwrap().is_empty());
    // A feature with no picture.
    let (mut rig, host, asked) = rig_with(claimed(), vec![answer()]).await;
    *host.files.lock().unwrap() = plan_files("new", false);
    assert!(reports(&mut rig, &asked, 2).await[1].get("needsDesign").is_none());
    assert!(host.prompts.lock().unwrap().is_empty());
    // A plan sent back (its pull request is open): the review it had is not asked for again.
    let sent_back = json!({ "id": "d1", "request": "Let people snooze", "titles": ["Snooze"], "sessionId": "s1",
        "prUrl": "https://github.com/o/r/pull/9", "notes": "Make it bigger" });
    let (mut rig, host, asked) = rig_with(sent_back, vec![answer()]).await;
    *host.files.lock().unwrap() = plan_files("new", true);
    assert!(reports(&mut rig, &asked, 2).await[1].get("needsDesign").is_none());
    assert!(host.prompts.lock().unwrap().is_empty());
}
