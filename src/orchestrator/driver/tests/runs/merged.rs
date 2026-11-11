//! The `merged` event a merge run writes when it lands (`ci-merged-events`).
use super::*;

/// The store's turn for a merge: the piece the run is on.
fn merge_run(token: &str, change: Value, chore: bool) -> Value {
    let mut r = turn(token);
    r["turn"]["piece"]["change"] = change;
    if chore {
        r["turn"]["piece"]["chore"] = json!({ "kind": "tidy" });
    }
    r
}

fn merged_events(rig: &mut Rig) -> Vec<Value> {
    (rig.lp(0).events().recent().iter())
        .map(Entry::to_value)
        .filter(|e| e["kind"] == "merged")
        .collect()
}

async fn merge_ends(change: Value, chore: bool, outcome: RunOutcome, ends: usize) -> Vec<Value> {
    let (mut rig, script) = setup(1).await;
    let starter = FakeRuns::new(&[RunKind::Merge]);
    let t0 = Instant::now();
    script.lock().unwrap().merges = VecDeque::from([merge_run("m1", change, chore)]);
    pass(&mut rig, &starter, t0).await;
    for _ in 0..ends {
        starter.finish("m1", outcome.clone());
    }
    pass(&mut rig, &starter, t0 + Duration::from_secs(1)).await;
    merged_events(&mut rig)
}

#[tokio::test]
async fn a_merged_run_writes_one_merged_event_with_the_item_and_the_pr_number() {
    let events = merge_ends(json!(42), false, RunOutcome::Merged, 1).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["item"], "piece-m1");
    assert_eq!(events[0]["pr"], 42);
    assert_eq!(events[0]["chore"], false);
}

#[tokio::test]
async fn a_pr_url_gives_its_number_and_a_chore_is_marked() {
    let change = json!("https://github.com/o/r/pull/77");
    let events = merge_ends(change, true, RunOutcome::Merged, 1).await;
    assert_eq!(
        (events[0]["pr"].as_u64(), events[0]["chore"].as_bool()),
        (Some(77), Some(true))
    );
    let events = merge_ends(
        json!({ "url": "https://github.com/o/r/pull/9/" }),
        false,
        RunOutcome::Merged,
        1,
    )
    .await;
    assert_eq!(events[0]["pr"], 9);
}

#[tokio::test]
async fn no_number_a_failed_merge_or_a_second_end_write_nothing_more() {
    assert!(
        merge_ends(json!("no number"), false, RunOutcome::Merged, 1)
            .await
            .is_empty()
    );
    let failed = RunOutcome::Failed("conflict".into());
    assert!(merge_ends(json!(42), false, failed, 1).await.is_empty());
    assert_eq!(merge_ends(json!(42), false, RunOutcome::Merged, 2).await.len(), 1);
}
