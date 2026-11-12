//! `step_start`'s key and `again`, and `step_end` (docs/specs/agent-telemetry.md section 3.3).
use super::*;
use crate::orchestrator::steps::CheckTime;

fn step_events(rig: &mut Rig, kind: &str) -> Vec<Value> {
    (rig.lp(0).events().recent().iter())
        .map(Entry::to_value)
        .filter(|e| e["kind"] == kind)
        .collect()
}

#[tokio::test]
async fn a_checking_step_writes_a_keyed_step_start_and_a_step_end() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    at(&mut rig, 0, "a", ItemState::Checking, Some(1));
    *fake.to.lock().unwrap() = Some(ItemState::Reviewing);
    rig.machine.poll().await;
    let start = step_events(&mut rig, "step_start").remove(0);
    assert_eq!(
        (start["step"].as_str(), start["key"].as_str()),
        (Some("checking"), Some("slice 1 round 1"))
    );
    assert!(start.get("again").is_none());
    settle().await;
    rig.machine.poll().await;
    let end = step_events(&mut rig, "step_end").remove(0);
    assert_eq!(
        (end["item"].as_str(), end["step"].as_str()),
        (Some("a"), Some("checking"))
    );
    assert_eq!(
        (end["key"].as_str(), end["ok"].as_bool(), end["outcome"].as_str()),
        (Some("slice 1 round 1"), Some(true), Some("reviewing"))
    );
    assert!(end["ms"].is_number());
}

#[tokio::test]
async fn a_step_started_while_one_is_open_is_again() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    at(&mut rig, 0, "a", ItemState::Checking, Some(1));
    rig.lp(0).step_started(
        "a",
        &ItemRun {
            state: ItemState::Checking,
            ..ItemRun::new()
        },
    );
    fake.hold.store(true, Ordering::SeqCst);
    rig.machine.poll().await;
    let starts = step_events(&mut rig, "step_start");
    assert_eq!(starts.len(), 2);
    assert!(starts[0].get("again").is_none());
    assert_eq!(starts[1]["again"], true);
}

#[tokio::test]
async fn a_panicking_or_cut_step_still_writes_step_end_with_ok_false() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    at(&mut rig, 0, "a", ItemState::Checking, Some(1));
    fake.step_panics.store(true, Ordering::SeqCst);
    rig.machine.poll().await;
    settle().await;
    fake.hold.store(true, Ordering::SeqCst);
    rig.machine.poll().await;
    let ends = step_events(&mut rig, "step_end");
    assert_eq!(
        (ends[0]["ok"].as_bool(), ends[0]["outcome"].as_str()),
        (Some(false), Some("panicked"))
    );
    // The retried step is held, then cut by a restart.
    rig.machine.stopping_override = Some(true);
    rig.machine.poll().await;
    let ends = step_events(&mut rig, "step_end");
    assert_eq!(ends.len(), 2);
    assert_eq!(
        (ends[1]["ok"].as_bool(), ends[1]["outcome"].as_str()),
        (Some(false), Some("cut"))
    );
}

fn two_checks() -> StepDetail {
    let check = |name: &str, ok, sec| CheckTime {
        name: name.into(),
        ok,
        sec,
    };
    StepDetail {
        checks: vec![check("lint", true, 1.5), check("test", false, 20.0)],
        ..Default::default()
    }
}

#[tokio::test]
async fn a_checking_step_with_checks_writes_them_in_step_end() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    *fake.detail.lock().unwrap() = two_checks();
    at(&mut rig, 0, "a", ItemState::Checking, Some(1));
    rig.machine.poll().await;
    settle().await;
    rig.machine.poll().await;
    let end = step_events(&mut rig, "step_end").remove(0);
    assert_eq!(
        end["checks"],
        json!([{"name": "lint", "ok": true, "sec": 1.5}, {"name": "test", "ok": false, "sec": 20.0}])
    );
}

#[tokio::test]
async fn a_step_with_no_detail_writes_no_checks() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    at(&mut rig, 0, "a", ItemState::Checking, Some(1));
    rig.machine.poll().await;
    settle().await;
    rig.machine.poll().await;
    assert!(step_events(&mut rig, "step_end").remove(0).get("checks").is_none());
}

fn review_detail() -> StepDetail {
    StepDetail {
        lines: Some(40),
        scouts: Some(2),
        ..Default::default()
    }
}

#[tokio::test]
async fn a_reviewing_step_writes_its_lines_and_scouts() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    *fake.detail.lock().unwrap() = review_detail();
    at(&mut rig, 0, "a", ItemState::Reviewing, Some(1));
    rig.machine.poll().await;
    settle().await;
    rig.machine.poll().await;
    let end = step_events(&mut rig, "step_end").remove(0);
    assert_eq!((end["lines"].as_u64(), end["scouts"].as_u64()), (Some(40), Some(2)));
}

#[tokio::test]
async fn a_checking_step_end_has_no_lines_or_scouts() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    *fake.detail.lock().unwrap() = two_checks();
    at(&mut rig, 0, "a", ItemState::Checking, Some(1));
    rig.machine.poll().await;
    settle().await;
    rig.machine.poll().await;
    let end = step_events(&mut rig, "step_end").remove(0);
    assert!(end.get("lines").is_none() && end.get("scouts").is_none());
}

#[tokio::test]
async fn a_panicked_review_step_end_has_no_lines_or_scouts() {
    let (base, _) = claim_stub(json!([]), Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    *fake.detail.lock().unwrap() = review_detail();
    fake.step_panics.store(true, Ordering::SeqCst);
    at(&mut rig, 0, "a", ItemState::Reviewing, Some(1));
    rig.machine.poll().await;
    settle().await;
    rig.machine.poll().await;
    let end = step_events(&mut rig, "step_end").remove(0);
    assert_eq!(
        (end["ok"].as_bool(), end["outcome"].as_str()),
        (Some(false), Some("panicked"))
    );
    assert!(end.get("lines").is_none() && end.get("scouts").is_none());
}
