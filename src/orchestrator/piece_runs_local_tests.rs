//! A chore and the local model's free slot (docs/specs/agent/07.2-local-model.md).
use super::*;

/// The model a `kind` chore's session is listed with, on a rig whose local model is `local` and `chore-rotation` On.
async fn model_of_chore(local: &crate::local_model::LocalModel, kind: &str) -> String {
    model_of_chore_with(local, kind, true, false).await
}

async fn model_of_chore_with(local: &crate::local_model::LocalModel, kind: &str, grab: bool, ladder: bool) -> String {
    crate::tiers::set_local_chores_scouts_for_test(grab);
    crate::tiers::set_local_ladder_for_test(ladder);
    let (url, _log) = lock_store("free").await;
    let mut r = rig(&[&format!("sleep 1; {COMMIT}")], &answer("pr_opened", ""));
    chore_rig(&mut r);
    Arc::get_mut(&mut r.starter).unwrap().local = local.clone();
    let (mut req, mut s) = request(&r, RunKind::Build, chore_piece(kind));
    on_store(&mut req, &url);
    r.starter.start(req).unwrap();
    let rows = crate::orchestrator::fake_session_tests::rows_once_running(&r.starter.sessions).await;
    let model = rows[0].info.model.clone();
    end_of(&mut s.ends).await;
    model
}

#[tokio::test]
async fn a_tidy_chore_takes_a_free_local_slot() {
    let local = crate::local_model::LocalModel::up();
    assert_eq!(model_of_chore(&local, "tidy").await, "local");
    assert!(local.try_take().is_some(), "the slot is free again after the chore");
}

#[tokio::test]
async fn a_tidy_chore_with_the_local_slot_busy_runs_on_the_chore_tier_at_once() {
    let local = crate::local_model::LocalModel::up();
    let _held = local.take().await;
    assert_eq!(model_of_chore(&local, "tidy").await, "sonnet");
}

#[tokio::test]
async fn a_playtest_fix_never_takes_the_local_slot() {
    let local = crate::local_model::LocalModel::up();
    assert_ne!(model_of_chore(&local, "playtest_fix").await, "local");
    assert!(local.try_take().is_some());
}

#[tokio::test]
async fn without_the_chores_flag_a_chore_never_takes_the_free_slot_even_on_the_ladder() {
    for ladder in [false, true] {
        let local = crate::local_model::LocalModel::up();
        assert_eq!(model_of_chore_with(&local, "tidy", false, ladder).await, "sonnet");
        assert!(local.try_take().is_some());
    }
}

#[tokio::test]
async fn a_tidy_chore_takes_the_free_slot_with_both_flags_on_too() {
    let local = crate::local_model::LocalModel::up();
    assert_eq!(model_of_chore_with(&local, "tidy", true, true).await, "local");
}
