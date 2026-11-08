//! The implementer's model tier (agent/07.1) and its `local` row (agent/07.2): the one slot, and the same reading
//! with local down or the flag Off.
use super::tests::{FINISHED, Rig, fake_name, request, rig, rig_with};
use super::*;
use crate::local_model::LocalModel;
use std::time::Duration;

/// A fake `claude` that records its arguments and the local base URL it was given, then answers `FINISHED`.
#[cfg(unix)]
fn recording_claude(rig: &Rig, sleep: u32) {
    let script = format!(
        "#!/bin/sh\necho \"$@ BASE=$ANTHROPIC_BASE_URL\" > \"{}\"\nsleep {sleep}\ncat > /dev/null\ncat '{}'\n",
        rig.args.display(),
        rig.root.join("out.jsonl").display()
    );
    crate::test_support::write_executable(&rig.root.join(fake_name()), &script);
}

/// A reading with 10% of the 5-hour limit left: below `sonnetAbovePct`, above `pauseBelowPct`.
fn low_usage() -> Option<Value> {
    Some(serde_json::json!({"unifiedWindows": {"five_hour": {"utilization": 0.9, "resetsAt": i64::MAX}}}))
}

#[cfg(unix)]
#[tokio::test]
async fn below_sonnet_above_pct_with_local_up_the_implementer_runs_on_the_local_model_one_at_a_time() {
    crate::tiers::set_local_ladder_for_test(true);
    let local = LocalModel::up();
    let rig = rig_with(Some(FINISHED), EngineChoiceConfig::default(), true, local.clone());
    recording_claude(&rig, 1);
    let held = local.take().await;
    let (mut request, mut rx) = request();
    request.usage = low_usage();
    rig.starter.start(request).unwrap();
    // Waits for the slot: nothing has started.
    tokio::time::sleep(Duration::from_millis(700)).await;
    let started = std::fs::read_to_string(&rig.args).unwrap_or_default();
    assert!(!started.contains("--model"), "claude started: {started}");
    assert!(rig.starter.sessions.snapshot().is_empty(), "listed");
    drop(held);
    let rows = crate::orchestrator::fake_session_tests::rows_once_running(&rig.starter.sessions).await;
    assert_eq!(rows[0].info.model, "local");
    assert!(local.try_take().is_none(), "the session holds the slot while it runs");
    tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(
        args.contains("--model sonnet") && !args.contains("--max-budget-usd"),
        "{args}"
    );
    assert!(args.contains("BASE=http://127.0.0.1:11436"), "{args}");
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(local.try_take().is_some(), "the slot is free again");
}

#[cfg(unix)]
#[tokio::test]
async fn with_local_down_or_the_flag_off_the_same_reading_gives_plain_sonnet() {
    for (flag, local) in [(true, LocalModel::off()), (false, LocalModel::up())] {
        crate::tiers::set_local_ladder_for_test(flag);
        let rig = rig_with(Some(FINISHED), EngineChoiceConfig::default(), true, local.clone());
        recording_claude(&rig, 0);
        let (mut request, mut rx) = request();
        request.usage = low_usage();
        rig.starter.start(request).unwrap();
        rx.recv().await.unwrap();
        let args = std::fs::read_to_string(&rig.args).unwrap();
        assert!(args.contains("--model sonnet") && !args.contains("BASE=http"), "{args}");
        assert!(local.try_take().is_some() || !flag);
    }
}

#[tokio::test]
async fn no_usage_reading_yet_still_starts_a_first_try_on_sonnet() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    let (mut request, mut rx) = request();
    request.usage = None;
    rig.starter.start(request).unwrap();
    rx.recv().await.unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    let model = args.split_whitespace().skip_while(|a| *a != "--model").nth(1);
    assert_eq!(model, Some("sonnet"), "{args}");
}

#[tokio::test]
async fn a_reopened_item_with_no_usage_reading_starts_on_opus() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    let (mut request, mut rx) = request();
    request.usage = None;
    request.item.reopened = Some(crate::backlog::Reopened {
        notes: Some("The list is empty".into()),
        at: Some("2026-10-29T00:00:00Z".into()),
        by: None,
    });
    rig.starter.start(request).unwrap();
    rx.recv().await.unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    let model = args.split_whitespace().skip_while(|a| *a != "--model").nth(1);
    assert_eq!(model, Some("opus"), "{args}");
}

#[tokio::test]
async fn a_usage_reading_below_opus_above_pct_starts_the_implementer_on_sonnet() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    let (mut request, mut rx) = request();
    request.usage = Some(serde_json::json!({"status": "allowed_warning", "resetsAt": i64::MAX}));
    rig.starter.start(request).unwrap();
    rx.recv().await.unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    let model = args.split_whitespace().skip_while(|a| *a != "--model").nth(1);
    assert_eq!(model, Some("sonnet"), "{args}");
}
