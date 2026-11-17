use super::*;

const P: &str = "proj";

fn inputs(switch: Option<bool>, rollout: bool, missing: Option<&str>) -> Inputs<'static> {
    Inputs {
        switch,
        repo: "octo/game",
        rollout,
        missing_tools: missing.map(str::to_string),
    }
}

fn why(r: &Resolved) -> &str {
    r.why.as_deref().unwrap_or("")
}

#[test]
fn before_the_hub_says_the_role_the_switch_and_the_rollout_decide() {
    let (gate, now) = (ReleaseGate::default(), Instant::now());
    assert!(gate.resolve(P, &inputs(None, true, None), now).on);
    assert!(gate.resolve(P, &inputs(Some(true), true, None), now).on);
    assert_eq!(
        gate.resolve(P, &inputs(Some(false), true, None), now),
        Resolved::off(None)
    );
    assert_eq!(gate.resolve(P, &inputs(None, false, None), now), Resolved::off(None));
}

#[test]
fn an_admin_owner_with_an_admin_login_is_on_by_default() {
    let (gate, now) = (ReleaseGate::default(), Instant::now());
    gate.set_role(P, Some(Role::Admin));
    assert_eq!(
        why(&gate.resolve(P, &inputs(None, true, None), now)),
        "Off: checking this computer's GitHub login for octo/game"
    );
    gate.record(P, true, now);
    assert_eq!(gate.resolve(P, &inputs(None, true, None), now), Resolved::on());
}

#[test]
fn a_member_is_off_even_with_the_switch_forced_on() {
    let (gate, now) = (ReleaseGate::default(), Instant::now());
    gate.record(P, true, now);
    for role in [Some(Role::Member), None] {
        gate.set_role(P, role);
        let r = gate.resolve(P, &inputs(Some(true), true, None), now);
        assert_eq!((r.on, why(&r)), (false, "Off: you're not an admin of octo/game"));
    }
}

#[test]
fn the_switch_off_says_no_reason_and_a_non_admin_login_says_its_own() {
    let (gate, now) = (ReleaseGate::default(), Instant::now());
    gate.set_role(P, Some(Role::Admin));
    gate.record(P, false, now);
    let r = gate.resolve(P, &inputs(None, true, None), now);
    assert_eq!(why(&r), "Off: this computer's GitHub login isn't an admin of octo/game");
    assert_eq!(
        gate.resolve(P, &inputs(Some(false), true, None), now),
        Resolved::off(None)
    );
}

#[test]
fn missing_rollout_tools_are_the_reason_once_the_checks_pass() {
    let (gate, now) = (ReleaseGate::default(), Instant::now());
    gate.set_role(P, Some(Role::Admin));
    gate.record(P, true, now);
    let r = gate.resolve(P, &inputs(None, true, Some("curl isn't installed")), now);
    assert_eq!((r.on, why(&r)), (false, "Off: curl isn't installed"));
}

#[test]
fn the_logins_answer_is_kept_thirty_minutes_then_reads_false() {
    let (gate, start) = (ReleaseGate::default(), Instant::now());
    gate.record(P, true, start);
    assert_eq!(gate.admin(P, start + ADMIN_KEEP - Duration::from_secs(1)), Some(true));
    assert_eq!(gate.admin(P, start + ADMIN_KEEP), Some(false));
    assert_eq!(gate.admin("other", start), None);
}

#[test]
fn a_changed_answer_wakes_the_hello_and_an_unchanged_one_does_not() {
    let (gate, now) = (ReleaseGate::default(), Instant::now());
    let mut woke = gate.subscribe();
    woke.mark_unchanged();
    assert!(gate.set_role(P, Some(Role::Admin)));
    assert!(woke.has_changed().unwrap());
    woke.mark_unchanged();
    assert!(!gate.set_role(P, Some(Role::Admin)));
    gate.record(P, true, now);
    assert!(woke.has_changed().unwrap());
    woke.mark_unchanged();
    gate.record(P, true, now + Duration::from_secs(60));
    assert!(!woke.has_changed().unwrap());
    // A failed check wakes it only once the last answer has lapsed.
    let at = now + Duration::from_secs(60);
    let was = gate.0.state.lock().unwrap().known(P, at);
    gate.lapse(P, was, at + Duration::from_secs(5));
    assert!(!woke.has_changed().unwrap());
    let late = at + ADMIN_KEEP;
    gate.lapse(P, Some(true), late);
    assert!(woke.has_changed().unwrap());
}

#[tokio::test]
async fn an_unknown_or_stale_answer_asks_the_check_to_run_now() {
    let (gate, now) = (ReleaseGate::default(), Instant::now());
    assert_eq!(gate.admin(P, now), None);
    tokio::time::timeout(Duration::from_secs(5), gate.0.kick.notified())
        .await
        .expect("the first read asks");
    gate.record(P, true, now);
    gate.admin(P, now + Duration::from_secs(60));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), gate.0.kick.notified())
            .await
            .is_err(),
        "a fresh answer asks nothing"
    );
    gate.admin(P, now + ADMIN_CHECK);
    tokio::time::timeout(Duration::from_secs(5), gate.0.kick.notified())
        .await
        .expect("a ten-minute-old answer asks again");
}

#[test]
fn the_hubs_role_words_are_read() {
    use serde_json::json;
    assert_eq!(Role::read(&json!("admin")), Some(Some(Role::Admin)));
    assert_eq!(Role::read(&json!("member")), Some(Some(Role::Member)));
    assert_eq!(Role::read(&json!(null)), Some(None));
    assert_eq!(Role::read(&json!("owner")), None);
    assert_eq!(Role::read(&json!(3)), None);
}
