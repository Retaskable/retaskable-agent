//! The draft specs a turn asks to file (`driver/experts.rs` `finish`, expert-chat.md section 5a): each filed as a plan for
//! the person to approve, after the turn's summary, and one the store refuses says why in the history.
use super::*;

fn notes(x: &Ex) -> Vec<String> {
    (x.st.args("report").iter())
        .filter_map(|n| n["note"].as_str().map(String::from))
        .collect()
}

#[tokio::test]
async fn the_specs_a_turn_asks_for_are_filed_as_drafts_by_the_person_after_its_summary() {
    let mut x = ex().await;
    *x.host.specs.lock().unwrap() = vec!["Piece one".into(), "Piece two".into()];
    x.start("s1", "fix it");
    x.until("both filed", |x| {
        notes(x).iter().filter(|n| n.starts_with("Draft spec filed:")).count() == 2
    })
    .await;
    let said = notes(&x);
    let at = |t: &str| {
        said.iter()
            .position(|n| n.starts_with(t))
            .unwrap_or_else(|| panic!("{t}: {said:?}"))
    };
    assert!(at("Did: fix it") < at("Draft spec filed: draft-1"), "{said:?}");
    let queued = x.st.args("draft_queue");
    assert_eq!(queued.len(), 2);
    assert_eq!(
        (queued[0]["request"].as_str(), queued[1]["request"].as_str()),
        (Some("Piece one"), Some("Piece two"))
    );
    assert_eq!(
        (queued[0]["requestedBy"].as_str(), queued[0]["assignee"].as_str()),
        (Some("ada"), Some("ada"))
    );
    assert_eq!(queued[0]["intake"], json!({ "chat": true }));
}

#[tokio::test]
async fn a_spec_the_store_refuses_says_why_in_the_history() {
    let mut x = ex().await;
    x.st.queue_refused.store(true, Ordering::SeqCst);
    *x.host.specs.lock().unwrap() = vec!["Piece one".into()];
    x.start("s1", "fix it");
    x.until("the refusal", |x| {
        notes(x).iter().any(|n| n.starts_with("A draft spec wasn't filed:"))
    })
    .await;
    assert!(!notes(&x).iter().any(|n| n.starts_with("Draft spec filed:")));
}
