//! The `ci` events a watch run sends are written to the project's log only with `ci-merged-events` On.
use super::*;

async fn written(on: bool) -> Vec<Value> {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(1, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(if on { &["ci-merged-events"] } else { &[] }));
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
async fn with_the_flag_on_the_loop_writes_the_event_and_off_it_does_not() {
    let on = written(true).await;
    assert_eq!(
        (on.len(), on[0]["item"].as_str(), on[0]["state"].as_str()),
        (1, Some("W1"), Some("pending"))
    );
    assert!(written(false).await.is_empty());
}
