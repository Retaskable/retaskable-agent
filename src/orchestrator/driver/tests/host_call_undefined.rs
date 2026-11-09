//! `host_call_undefined`: a `gh` call with an undefined argument is refused and written to the project's log,
//! behind `host-call-undefined-event`.
#![cfg(unix)]
use super::*;
use crate::telemetry::Event;

/// The sink and the guard are process-wide, so these tests take turns.
static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Runs a fake `gh` with `pr checks undefined`, then one pass; returns whether the fake ran, whether the call
/// erred, the project's `host_call_undefined` events and its anomaly ids.
async fn run(on: bool) -> (bool, bool, Vec<Value>, Vec<String>) {
    let _turn = TURN.lock().await;
    let (base, _) = claim_stub(json!([]), None).await;
    let mut rig = rig(1, &[&base]);
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("ran");
    let gh = dir.path().join("fake_gh.sh");
    crate::test_support::write_executable(&gh, &format!("#!/bin/sh\ntouch '{}'\n", marker.display()));
    std::fs::remove_file(&marker).unwrap(); // `write_executable` ran it once
    let flags: &[&str] = if on {
        &["host-call-undefined-event", "watchdog-anomaly-rules"]
    } else {
        &["watchdog-anomaly-rules"]
    };
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(flags));
    rig.lp(0).events = EventLog::new();
    rig.lp(0).project.repo_path = dir.path().to_string_lossy().into_owned();
    rig.go_live();
    rig.machine.pass().await; // publishes the On repos
    let err = crate::ci_watch::gh_out(&gh, dir.path(), &["pr", "checks", "undefined"], &|_| {}).is_err();
    rig.machine.pass().await;
    let events = (rig.lp(0).events.recent().iter())
        .map(Entry::to_value)
        .filter(|e| e["kind"] == "host_call_undefined")
        .collect();
    let anomalies = (rig.lp(0).events.recent().iter())
        .filter_map(|e| match e.event() {
            Some(Event::Anomaly(a)) => a.id.clone(),
            _ => None,
        })
        .collect();
    (marker.exists(), err, events, anomalies)
}

#[tokio::test]
async fn with_the_flag_on_the_call_is_refused_and_written_and_the_anomaly_opens() {
    let (ran, err, events, anomalies) = run(true).await;
    assert!(!ran && err, "ran={ran} err={err}");
    assert_eq!(events.len(), 1);
    assert_eq!(
        (events[0]["cmd"].as_str(), &events[0]["args"]),
        (Some("gh"), &json!(["pr", "checks", "undefined"]))
    );
    assert!(
        anomalies.iter().any(|a| a == "repeat_error:host_call_undefined"),
        "{anomalies:?}"
    );
}

#[tokio::test]
async fn with_the_flag_off_the_call_runs_and_nothing_is_written() {
    let (ran, err, events, _) = run(false).await;
    assert!(ran && !err);
    assert!(events.is_empty());
}

#[tokio::test]
async fn a_project_with_the_flag_off_keeps_today_s_behaviour_beside_one_that_is_on() {
    let _turn = TURN.lock().await;
    let (base, _) = claim_stub(json!([]), None).await;
    let mut rig = rig(2, &[&base, &base]);
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let marker = dir.path().join("ran");
    let gh = dir.path().join("fake_gh.sh");
    crate::test_support::write_executable(&gh, &format!("#!/bin/sh\ntouch '{}'\n", marker.display()));
    std::fs::remove_file(&marker).unwrap();
    for (i, d, flag) in [(0, &a, "host-call-undefined-event"), (1, &b, "watchdog-anomaly-rules")] {
        rig.lp(i).flags = Arc::new(crate::flags::Flags::fixed(&[flag]));
        rig.lp(i).events = EventLog::new();
        rig.lp(i).project.repo_path = d.to_string_lossy().into_owned();
    }
    rig.go_live();
    rig.machine.pass().await; // publishes the On repos
    assert!(crate::ci_watch::gh_out(&gh, &b, &["pr", "checks", "undefined"], &|_| {}).is_ok());
    assert!(marker.exists());
    assert!(crate::ci_watch::gh_out(&gh, &a, &["pr", "checks", "undefined"], &|_| {}).is_err());
    rig.machine.pass().await;
    let mut n = |i| {
        (rig.lp(i).events.recent().iter())
            .filter(|e| e.to_value()["kind"] == "host_call_undefined")
            .count()
    };
    assert_eq!((n(0), n(1)), (1, 0));
}
