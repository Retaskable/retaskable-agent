//! The `ci` events a watch run sends are written to the project's log.
use super::*;

async fn written() -> Vec<Value> {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(1, &[&base]);
    rig.lp(0)
        .run_loop
        .events_tx
        .send(crate::telemetry::Event::Ci(crate::telemetry::event::Ci {
            item: Some("W1".into()),
            state: Some("pending".into()),
            ms: Some(5.0),
            ..Default::default()
        }));
    rig.lp(0).write_run_events().await;
    (rig.lp(0).events().recent().iter())
        .map(Entry::to_value)
        .filter(|e| e["kind"] == "ci")
        .collect()
}

#[tokio::test]
async fn the_loop_writes_the_event() {
    let on = written().await;
    assert_eq!(
        (on.len(), on[0]["item"].as_str(), on[0]["state"].as_str()),
        (1, Some("W1"), Some("pending"))
    );
}
