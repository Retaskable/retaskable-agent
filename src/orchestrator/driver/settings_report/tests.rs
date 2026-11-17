use super::*;
use crate::backlog::store_contract::check_settings_report;

#[test]
fn the_report_is_what_the_store_takes() {
    let args = report_args("my-pc", 3, 7);
    assert_eq!(
        args,
        json!({ "machine": "my-pc", "allowed": 3, "appliedVersion": 7, "migrated": true })
    );
    check_settings_report(&args).unwrap();
    // 0 before anything is allowed, and a long machine name is cut to what the store takes.
    check_settings_report(&report_args(&"m".repeat(500), 0, 0)).unwrap();
}

#[test]
fn the_contract_check_refuses_what_the_store_refuses() {
    for bad in [
        json!({ "machine": "", "allowed": 0 }),
        json!({ "machine": "pc", "allowed": -1 }),
        json!({ "machine": "pc", "allowed": 1.5 }),
        json!({ "machine": "pc" }),
        json!({ "machine": "pc", "allowed": 1, "appliedVersion": "7" }),
        json!({ "machine": "pc", "allowed": 1, "migrated": "yes" }),
        json!({ "machine": "pc", "allowed": 1, "applied_version": 7 }),
    ] {
        assert!(check_settings_report(&bad).is_err(), "{bad}");
    }
}

#[test]
fn nothing_is_sent_before_the_store_has_settings_and_the_same_report_waits_a_day() {
    let mut s = SettingsReport::default();
    let t0 = Instant::now();
    assert!(s.due("pc", 0, t0).is_none(), "no settings read yet");
    s.applied = Some(4);
    let (args, key) = s.due("pc", 2, t0).unwrap();
    assert_eq!(args["allowed"], 2);
    s.sent = Some((key, t0));
    assert!(s.due("pc", 2, t0 + Duration::from_secs(3600)).is_none());
    assert!(
        s.due("pc", 3, t0 + Duration::from_secs(3600)).is_some(),
        "a newly allowed version"
    );
    s.applied = Some(5);
    assert!(
        s.due("pc", 2, t0 + Duration::from_secs(3600)).is_some(),
        "a newly applied version"
    );
    assert!(
        s.due("pc", 2, t0 + REPORT_EVERY).is_some(),
        "a day on, so it stays counted"
    );
}
