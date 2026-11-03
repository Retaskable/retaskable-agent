//! Item commands the loop carries out (`driver/item_commands.rs`): Continue, Start over and Skip.
use super::*;
use crate::view::ItemCommand;

/// A live rig whose item "a" stopped (`needs_human`) after two rounds, holding no lane; `session` is the
/// implementer session it left behind.
fn stopped(lanes: u32, session: Option<&str>) -> (Rig, Arc<Fake>) {
    stopped_at(lanes, session, "http://127.0.0.1:1")
}

fn stopped_at(lanes: u32, session: Option<&str>, base: &str) -> (Rig, Arc<Fake>) {
    let mut rig = rig(lanes, &[base]);
    let fake = rig.go_live();
    rig.queue(0, &["a"]);
    let lp = rig.lp(0);
    let run = lp.runs.get_mut("a").unwrap();
    run.state = ItemState::NeedsHuman;
    run.check_rounds = 2;
    run.review_rounds = 1;
    lp.leases.insert("a".into(), "lease-1".into());
    lp.records.insert(
        "a".into(),
        RunRecord {
            state: "needs_human".into(),
            lane: None,
            impl_session_id: session.map(Into::into),
            impl_session_engine: Default::default(),
            impl_context_tokens: None,
            branch: Some("a".into()),
            slice: 0,
            attempt: 0,
            ..Default::default()
        },
    );
    (rig, fake)
}

fn continue_a(rig: &Rig, item: &str, notes: Option<&str>) {
    rig.machine.links.item_commands.push(ItemCommand::Continue {
        project: "p0".into(),
        item: item.into(),
        notes: notes.map(Into::into),
    });
}

#[tokio::test]
async fn continue_with_notes_restarts_the_implementer_on_its_branch_with_the_notes() {
    let (mut rig, fake) = stopped(2, Some("s1"));
    continue_a(&rig, "a", Some("Use the other API."));
    rig.machine.poll().await;
    let started = fake.started.lock().unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].follow_up.as_deref(), Some("Use the other API."));
    assert!(!started[0].resume);
    drop(started);
    let run = rig.lp(0).runs["a"];
    assert_eq!(run.state, ItemState::Implementing);
    assert_eq!((run.check_rounds, run.review_rounds), (0, 0));
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    let moved = rig
        .lp(0)
        .events()
        .recent()
        .iter()
        .map(Entry::to_value)
        .find(|e| e["kind"] == "item_state")
        .unwrap();
    assert_eq!(
        (moved["from"].clone(), moved["to"].clone()),
        (json!("needs_human"), json!("implementing"))
    );
}

#[tokio::test]
async fn continue_without_notes_puts_the_item_back_to_run_its_checks() {
    let (mut rig, fake) = stopped(2, Some("s1"));
    continue_a(&rig, "a", None);
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty());
    let run = rig.lp(0).runs["a"];
    assert_eq!((run.state, run.check_rounds), (ItemState::Checking, 0));
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), Some(1));
    assert_eq!(fake.stepped.lock().unwrap().len(), 1, "its checks run");
}

#[tokio::test]
async fn continue_drops_an_item_that_is_unknown_or_not_stopped() {
    let (mut rig, fake) = stopped(2, Some("s1"));
    rig.queue(0, &["b"]);
    rig.lp(0).runs.get_mut("b").unwrap().state = ItemState::Implementing;
    continue_a(&rig, "b", Some("x"));
    continue_a(&rig, "nope", Some("x"));
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).runs["b"].state, ItemState::Implementing);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert!(rig.lp(0).continuing.is_empty());
    assert_eq!(rig.machine.pool.held_lanes().count(), 0);
}

fn start_over_a(rig: &Rig, item: &str, notes: Option<&str>) {
    rig.machine.links.item_commands.push(ItemCommand::StartOver {
        project: "p0".into(),
        item: item.into(),
        notes: notes.map(Into::into),
    });
}

/// "a" has a PR and is on slice 3 of the store's view.
fn with_pr(rig: &mut Rig) {
    let item = rig.lp(0).items.get_mut("a").unwrap();
    item.pr_url = Some("https://github.com/o/p/pull/3".into());
    item.slice = 3;
    rig.lp(0).runs.get_mut("a").unwrap().slice = 3;
}

#[tokio::test]
async fn continue_with_nothing_to_carry_on_from_starts_the_slice_over_notes_and_all() {
    let (mut rig, fake) = stopped(2, None);
    continue_a(&rig, "a", Some("Try it differently."));
    rig.machine.poll().await;
    let started = fake.started.lock().unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(
        (
            started[0].attempt,
            started[0].notes.as_deref(),
            started[0].follow_up.as_deref()
        ),
        (1, Some("Try it differently."), None)
    );
    drop(started);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert!(rig.lp(0).continuing.is_empty());
}

fn notes_of(rig: &mut Rig) -> Vec<String> {
    (rig.lp(0).events().recent().iter())
        .filter_map(|e| e.to_value()["note"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test]
async fn a_persons_continue_with_nothing_to_carry_on_from_says_so_in_its_note() {
    let (mut rig, _fake) = stopped(2, None);
    continue_a(&rig, "a", None);
    rig.machine.poll().await;
    let notes = notes_of(&mut rig);
    assert!(
        notes
            .iter()
            .any(|n| n == "Started over: there was nothing to continue from"),
        "{notes:?}"
    );
}

#[tokio::test]
async fn claudes_continue_with_nothing_to_carry_on_from_says_so_in_its_note() {
    let (mut rig, _fake) = stopped(2, None);
    rig.lp(0).continuing.push(("a".into(), Some("Try again.".into())));
    rig.lp(0).claude_pending.insert("a".into());
    rig.machine.poll().await;
    let notes = notes_of(&mut rig);
    assert!(
        notes
            .iter()
            .any(|n| n == "Started over by Claude: there was nothing to continue from"),
        "{notes:?}"
    );
}

#[tokio::test]
async fn a_plain_start_over_still_says_who_chose_it() {
    let (mut rig, _fake) = stopped(2, None);
    start_over_a(&rig, "a", None);
    rig.machine.poll().await;
    let notes = notes_of(&mut rig);
    assert!(notes.iter().any(|n| n == "a person chose Start over"), "{notes:?}");
}

#[tokio::test]
async fn start_over_closes_the_old_pr_bumps_the_attempt_and_starts_a_fresh_session_with_the_notes() {
    let (mut rig, fake) = stopped(2, Some("s1"));
    with_pr(&mut rig);
    start_over_a(&rig, "a", Some("Use the other API."));
    rig.machine.poll().await;
    assert_eq!(
        *fake.abandoned.lock().unwrap(),
        vec![Abandon {
            project: "p0".into(),
            branch: Some("a".into()),
            pr_url: Some("https://github.com/o/p/pull/3".into()),
        }]
    );
    let started = fake.started.lock().unwrap();
    assert_eq!(started.len(), 1);
    assert_eq!(
        (
            started[0].attempt,
            started[0].notes.as_deref(),
            started[0].follow_up.as_deref(),
            started[0].resume
        ),
        (1, Some("Use the other API."), None, false)
    );
    assert_ne!(started[0].session_id, "s1", "a fresh session, not the old one");
    drop(started);
    let run = rig.lp(0).runs["a"];
    assert_eq!(
        (run.state, run.check_rounds, run.review_rounds),
        (ItemState::Implementing, 0, 0)
    );
    let record = &rig.lp(0).records["a"];
    assert_eq!((record.attempt, record.slice), (1, 3));
    assert!(
        record.branch.as_deref().unwrap().ends_with("-s3-r1"),
        "{:?}",
        record.branch
    );
}

#[tokio::test]
async fn start_over_a_second_time_is_the_next_attempt_and_without_a_pr_drops_the_branch() {
    let (mut rig, fake) = stopped(2, Some("s1"));
    rig.lp(0).records.get_mut("a").unwrap().attempt = 1;
    start_over_a(&rig, "a", None);
    rig.machine.poll().await;
    let abandoned = fake.abandoned.lock().unwrap().clone();
    assert_eq!(
        (abandoned[0].pr_url.clone(), abandoned[0].branch.clone()),
        (None, Some("a".into()))
    );
    let started = fake.started.lock().unwrap();
    assert_eq!((started[0].attempt, started[0].notes.clone()), (2, None));
}

#[tokio::test]
async fn start_over_refuses_an_item_that_is_not_stopped_or_unknown() {
    let (mut rig, fake) = stopped(2, Some("s1"));
    rig.queue(0, &["b"]);
    rig.lp(0).runs.get_mut("b").unwrap().state = ItemState::Implementing;
    start_over_a(&rig, "b", Some("x"));
    start_over_a(&rig, "nope", Some("x"));
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty() && fake.abandoned.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).runs["b"].state, ItemState::Implementing);
    assert!(rig.lp(0).starting_over.is_empty());
    assert_eq!(rig.machine.pool.held_lanes().count(), 0);
}

#[tokio::test]
async fn start_over_that_cannot_let_go_of_the_old_work_leaves_the_item_as_it_is() {
    let (mut rig, fake) = stopped(2, Some("s1"));
    *fake.abandon_fails.lock().unwrap() = Some("gh isn't signed in".into());
    start_over_a(&rig, "a", None);
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty());
    let run = rig.lp(0).runs["a"];
    assert_eq!((run.state, run.check_rounds), (ItemState::NeedsHuman, 2));
    assert_eq!(rig.lp(0).records["a"].attempt, 0);
    assert_eq!(rig.machine.pool.held_lanes().count(), 0);
}

#[tokio::test]
async fn start_over_waits_for_a_free_lane_before_closing_anything() {
    let (mut rig, fake) = stopped(1, Some("s1"));
    let holder = Holder::Item {
        project: "p9".into(),
        item: "other".into(),
    };
    assert!(rig.machine.pool.take(1, holder.clone()));
    start_over_a(&rig, "a", None);
    rig.machine.poll().await;
    assert!(fake.abandoned.lock().unwrap().is_empty() && fake.started.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).starting_over.len(), 1);
    assert!(rig.machine.pool.release(1, &holder));
    rig.machine.poll().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn continue_waits_for_a_free_lane_and_starts_once_one_frees() {
    let (mut rig, fake) = stopped(1, Some("s1"));
    let holder = Holder::Item {
        project: "p9".into(),
        item: "other".into(),
    };
    assert!(rig.machine.pool.take(1, holder.clone()));
    continue_a(&rig, "a", Some("Go on."));
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty());
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert_eq!(rig.lp(0).continuing.len(), 1);

    assert!(rig.machine.pool.release(1, &holder));
    rig.machine.poll().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    assert!(rig.lp(0).continuing.is_empty());
}

fn skip_a(rig: &Rig, item: &str) {
    rig.machine.links.item_commands.push(ItemCommand::Skip {
        project: "p0".into(),
        item: item.into(),
    });
}

#[tokio::test]
async fn skip_drops_a_stopped_item_releases_its_lease_and_skips_it_in_the_store() {
    let (base, asked) = claim_stub(json!([]), None).await;
    let (mut rig, fake) = stopped_at(2, Some("s1"), &base);
    skip_a(&rig, "a");
    rig.machine.poll().await;
    assert!(fake.started.lock().unwrap().is_empty());
    assert!(!rig.lp(0).runs.contains_key("a"));
    assert!(!rig.lp(0).leases.contains_key("a"));
    assert!(rig.lp(0).unreleased.is_empty());
    assert_eq!(ops(&asked)[..3], ["report", "release", "skip"]);
    let sent = asked.lock().unwrap();
    assert_eq!(sent[0].1["note"], "A person chose Skip.");
    assert_eq!(sent[2].1, json!({ "id": "a" }));
    drop(sent);
    let moved = rig
        .lp(0)
        .events()
        .recent()
        .iter()
        .map(Entry::to_value)
        .find(|e| e["kind"] == "item_state")
        .unwrap();
    assert_eq!(
        (moved["from"].clone(), moved["to"].clone()),
        (json!("needs_human"), json!("skipped"))
    );
}

#[tokio::test]
async fn skip_stays_owed_while_the_release_is_refused_then_goes_through() {
    let fail = Arc::new(AtomicBool::new(true));
    let (base, asked) = claim_stub_with(json!([]), None, fail.clone()).await;
    let (mut rig, _fake) = stopped_at(2, Some("s1"), &base);
    skip_a(&rig, "a");
    rig.machine.poll().await;
    assert!(!ops(&asked).contains(&"skip".to_string()));
    assert!(rig.lp(0).unreleased.contains_key("a"));
    assert!(rig.lp(0).unskipped.contains("a"));
    fail.store(false, Ordering::SeqCst);
    asked.lock().unwrap().clear();
    rig.machine.pass().await;
    let sent = ops(&asked);
    let (release, skip) = (
        sent.iter().position(|o| o == "release"),
        sent.iter().position(|o| o == "skip"),
    );
    assert!(release.is_some() && release < skip, "{sent:?}");
    assert!(rig.lp(0).unreleased.is_empty() && rig.lp(0).unskipped.is_empty());
}

#[tokio::test]
async fn skip_stays_owed_when_only_the_skip_is_refused_then_goes_through() {
    let fail_skip = Arc::new(AtomicBool::new(true));
    let (base, asked) = claim_stub_failing(json!([]), None, Default::default(), fail_skip.clone()).await;
    let (mut rig, _fake) = stopped_at(2, Some("s1"), &base);
    skip_a(&rig, "a");
    rig.machine.poll().await;
    assert!(rig.lp(0).unreleased.is_empty());
    assert!(rig.lp(0).unskipped.contains("a"));
    fail_skip.store(false, Ordering::SeqCst);
    asked.lock().unwrap().clear();
    rig.machine.pass().await;
    let sent = ops(&asked);
    assert!(
        sent.contains(&"skip".to_string()) && !sent.contains(&"release".to_string()),
        "{sent:?}"
    );
    assert!(rig.lp(0).unskipped.is_empty());
}

#[tokio::test]
async fn skip_drops_an_item_that_is_unknown_or_not_stopped() {
    let (base, asked) = claim_stub(json!([]), None).await;
    let (mut rig, _fake) = stopped_at(2, Some("s1"), &base);
    rig.queue(0, &["b"]);
    rig.lp(0).runs.get_mut("b").unwrap().state = ItemState::Implementing;
    skip_a(&rig, "b");
    skip_a(&rig, "nope");
    rig.machine.poll().await;
    assert_eq!(rig.lp(0).runs["b"].state, ItemState::Implementing);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert!(!ops(&asked).contains(&"skip".to_string()));
}
