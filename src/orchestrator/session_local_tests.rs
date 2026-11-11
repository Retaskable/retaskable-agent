//! The implementer's model tier (agent/07.1) and its `local` row (agent/07.2): the one slot, and the same reading
//! with local down.
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

#[cfg(unix)]
#[tokio::test]
async fn below_sonnet_above_pct_with_local_up_the_implementer_runs_on_the_local_model_one_at_a_time() {
    let local = LocalModel::up();
    let rig = rig_with(Some(FINISHED), EngineChoiceConfig::default(), true, local.clone());
    recording_claude(&rig, 1);
    let held = local.take().await;
    let (mut request, mut rx) = request();
    request.item.tier = Some("local".into());
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
async fn with_local_down_the_same_reading_gives_plain_sonnet() {
    let local = LocalModel::off();
    let rig = rig_with(Some(FINISHED), EngineChoiceConfig::default(), true, local.clone());
    recording_claude(&rig, 0);
    let (mut request, mut rx) = request();
    request.item.tier = Some("local".into());
    rig.starter.start(request).unwrap();
    rx.recv().await.unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(args.contains("--model sonnet") && !args.contains("BASE=http"), "{args}");
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
async fn a_reopened_item_the_store_named_opus_starts_on_opus() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    let (mut request, mut rx) = request();
    request.item.tier = Some("opus".into());
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

/// A rig whose implementer role is set to the `local` engine.
fn local_engine_rig(local: &LocalModel) -> Rig {
    let mut choice = EngineChoiceConfig::default();
    choice.roles.implementer = Some(Engine::Local);
    let rig = rig_with(Some(FINISHED), choice, true, local.clone());
    recording_claude(&rig, 0);
    rig
}

#[cfg(unix)]
#[tokio::test]
async fn an_engine_set_to_local_starts_on_the_local_target_even_with_a_full_window() {
    let local = LocalModel::up();
    let rig = local_engine_rig(&local);
    let (mut request, mut rx) = request();
    request.usage = None;
    rig.starter.start(request).unwrap();
    rx.recv().await.unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(
        args.contains("--model sonnet") && args.contains("BASE=http://127.0.0.1:11436"),
        "{args}"
    );
    assert!(!args.contains("--max-budget-usd"), "{args}");
}

#[cfg(unix)]
#[tokio::test]
async fn an_engine_set_to_local_with_local_down_runs_on_the_roles_tier() {
    let rig = local_engine_rig(&LocalModel::off());
    let (mut request, mut rx) = request();
    request.item.tier = Some("opus".into());
    request.item.reopened = Some(crate::backlog::Reopened {
        notes: Some("x".into()),
        at: Some("2026-10-29T00:00:00Z".into()),
        by: None,
    });
    rig.starter.start(request).unwrap();
    rx.recv().await.unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(args.contains("--model opus") && !args.contains("BASE=http"), "{args}");
}

/// Runs a first try (which makes the lane), then swaps its transcript's first line for `first` (none when `None`) and
/// resumes it with local up or down; returns the arguments the fake `claude` got on the resume.
#[cfg(unix)]
async fn resume_after(first: Option<&str>, local: LocalModel) -> String {
    resume_limited(first, local, false).await
}

/// [`resume_after`], with Claude limited on this computer before the resume when `limited`.
#[cfg(unix)]
async fn resume_limited(first: Option<&str>, local: LocalModel, limited: bool) -> String {
    let rig = rig_with(Some(FINISHED), EngineChoiceConfig::default(), true, local);
    recording_claude(&rig, 0);
    let (first_try, mut rx) = request();
    let file = paths::run_dir(&rig.starter.data_dir, &first_try.project, &first_try.item.id).join("sess-1.jsonl");
    rig.starter.start(first_try).unwrap();
    rx.recv().await.unwrap();
    match first {
        Some(line) => std::fs::write(&file, format!("{line}\n")).unwrap(),
        None => std::fs::remove_file(&file).unwrap(),
    }
    if limited {
        rig.starter
            .sessions
            .limits()
            .hit(EngineId::Claude, std::time::Instant::now() + Duration::from_secs(600));
    }
    let (mut request, mut rx) = request();
    request.item.tier = Some("local".into());
    request.resume = true;
    rig.starter.start(request).unwrap();
    rx.recv().await.unwrap();
    std::fs::read_to_string(&rig.args).unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn a_local_transcript_resumes_locally_while_local_is_up_and_on_its_tier_when_down() {
    let line = r#"{"type":"retaskable_meta","engine":"claude","model":"local"}"#;
    let up = resume_after(Some(line), LocalModel::up()).await;
    assert!(
        up.contains("BASE=http://127.0.0.1:11436") && up.contains("--resume"),
        "{up}"
    );
    let down = resume_after(Some(line), LocalModel::off()).await;
    assert!(
        !down.contains("BASE=http://") && down.contains("--model sonnet"),
        "{down}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn an_anthropic_or_missing_transcript_resumes_on_sonnet_never_locally() {
    let line = r#"{"type":"retaskable_meta","engine":"claude","model":"opus"}"#;
    for first in [Some(line), None] {
        let args = resume_after(first, LocalModel::up()).await;
        assert!(
            args.contains("--model sonnet") && !args.contains("BASE=http://"),
            "{args}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_start_over_job_the_store_named_opus_starts_on_opus() {
    for (attempt, model) in [(1, "--model opus"), (0, "--model sonnet")] {
        let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
        recording_claude(&rig, 0);
        let (mut request, mut rx) = request();
        request.attempt = attempt;
        request.item.tier = Some("opus".into());
        rig.starter.start(request).unwrap();
        tokio::time::timeout(Duration::from_secs(120), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let args = std::fs::read_to_string(&rig.args).unwrap();
        assert!(args.contains(model), "attempt {attempt}: {args}");
    }
}

/// A reading that says `allowed` with room to spare, so the saved usage alone gives an Anthropic tier.
fn allowed_usage() -> Option<Value> {
    Some(serde_json::json!({"status": "allowed", "resetsAt": i64::MAX}))
}

/// Starts a fresh session with Claude limited and `allowed` saved usage; returns the arguments `claude` got.
#[cfg(unix)]
async fn limited_start(local: LocalModel) -> String {
    let rig = rig_with(Some(FINISHED), EngineChoiceConfig::default(), true, local);
    recording_claude(&rig, 0);
    rig.starter
        .sessions
        .limits()
        .hit(EngineId::Claude, std::time::Instant::now() + Duration::from_secs(600));
    let (mut request, mut rx) = request();
    request.usage = allowed_usage();
    rig.starter.start(request).unwrap();
    rx.recv().await.unwrap();
    std::fs::read_to_string(&rig.args).unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn claude_at_its_limit_starts_a_fresh_session_on_the_local_model_whatever_the_saved_reading_says() {
    let args = limited_start(LocalModel::up()).await;
    assert!(
        args.contains("BASE=http://127.0.0.1:11436") && args.contains("--model sonnet"),
        "{args}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn with_local_down_a_limited_claude_starts_on_its_tier() {
    let args = limited_start(LocalModel::off()).await;
    assert!(args.contains("--model sonnet") && !args.contains("BASE=http"), "{args}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_resumed_anthropic_session_never_takes_the_model_at_the_limit() {
    let line = r#"{"type":"retaskable_meta","engine":"claude","model":"opus"}"#;
    let args = resume_limited(Some(line), LocalModel::up(), true).await;
    assert!(
        args.contains("--model sonnet") && !args.contains("BASE=http://"),
        "{args}"
    );
}
