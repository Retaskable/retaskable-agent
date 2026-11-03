//! A stopped item gets one look from a triager (`driver/triage_run.rs`, 06.9 bullets 16 and 20).
use super::*;
use crate::config::OnOff;
use crate::triage::TriageDecision;

/// The triager sessions the fake starter was asked to start, and how each ends, in order (one with no scripted end
/// never reports).
#[derive(Default)]
pub(super) struct Triager {
    asked: Mutex<Vec<TriageRequest>>,
    ends: Mutex<Vec<TriageEnd>>,
}

impl Triager {
    pub(super) fn start(&self, request: TriageRequest) -> Result<(), String> {
        self.asked.lock().unwrap().push(request.clone());
        let mut ends = self.ends.lock().unwrap();
        if !ends.is_empty() {
            request.outcomes.send(SessionOutcome {
                project: request.project,
                item_id: request.item_id,
                lane: request.lane.unwrap_or(0),
                result: SessionResult::Triage(ends.remove(0)),
                context_tokens: None,
                usage: None,
                notes: vec![],
                session_id: None,
            });
        }
        Ok(())
    }
}

/// "a" claimed and its session ended by `result`, not yet read.
async fn settling_ended(result: SessionResult) -> (Rig, Arc<Fake>, Asked) {
    let (base, asked) = claim_stub_with(json!([item("a", "claimed")]), handed_out(), Default::default()).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["triage-session"]));
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(result);
    rig.machine.pass().await;
    (rig, fake, asked)
}

fn found(reason: &str) -> TriageEnd {
    TriageEnd::Found {
        decision: TriageDecision::Human,
        reason: reason.into(),
        steps: vec![],
        instructions: String::new(),
    }
}

fn failed(why: LookFailure) -> TriageEnd {
    TriageEnd::Failed {
        note: "the session crashed".into(),
        why,
    }
}

/// A rig with "a" stopped at `needs_human` ("first problem"), the triager's ends scripted as `ends`, and no look
/// asked for yet (the pass that settled the stop is the one that asks, so it has run).
async fn stopped(ends: Vec<TriageEnd>) -> (Rig, Arc<Fake>) {
    let (mut rig, fake, _asked) = settling_ended(SessionResult::NeedsHuman("first problem".into())).await;
    *fake.triage.ends.lock().unwrap() = ends;
    rig.machine.pass().await;
    (rig, fake)
}

fn looks(fake: &Fake) -> usize {
    fake.triage.asked.lock().unwrap().len()
}

fn on_it_events(rig: &mut Rig) -> Vec<Option<bool>> {
    (rig.lp(0).events().recent().iter())
        .map(Entry::to_value)
        .filter(|e| e["kind"] == "item_state" && !e["claudeOnIt"].is_null())
        .map(|e| e["claudeOnIt"].as_bool())
        .collect()
}

fn past(rig: &mut Rig) {
    rig.lp(0).records.get_mut("a").unwrap().next_look_at = Some("2000-01-01T00:00:00Z".into());
}

#[tokio::test]
async fn a_stopped_item_gets_exactly_one_look_that_leaves_a_human_finding_on_its_reason_and_timeline() {
    let (mut rig, fake) = stopped(vec![found("it needs a design call")]).await;
    for _ in 0..3 {
        rig.machine.pass().await;
    }
    assert_eq!(looks(&fake), 1);
    let req = fake.triage.asked.lock().unwrap()[0].clone();
    assert_eq!(
        (req.item_id.as_str(), req.context.reason.as_str()),
        ("a", "first problem")
    );
    let r = rig.lp(0).records["a"].clone();
    assert!(r.triaged);
    let reason = r.needs_human_reason.unwrap();
    assert!(reason.starts_with("first problem"), "{reason}");
    assert!(
        reason.contains("Claude looked into it: it needs a design call"),
        "{reason}"
    );
    assert!(
        rig.lp(0)
            .item_notes("a", 10)
            .iter()
            .any(|n| n.contains("it needs a design call"))
    );
}

#[tokio::test]
async fn person_steps_are_left_as_a_human_finding_with_the_steps_listed_under_the_reason() {
    let end = TriageEnd::Found {
        decision: TriageDecision::PersonSteps,
        reason: "only you can".into(),
        steps: vec!["Add the key".into(), "Press Done".into()],
        instructions: String::new(),
    };
    let (mut rig, _fake) = stopped(vec![end]).await;
    rig.lp(0).items.get_mut("a").unwrap().pr_url = Some("https://github.com/x/y/pull/1".into());
    rig.machine.pass().await;
    let reason = rig.lp(0).records["a"].needs_human_reason.clone().unwrap();
    assert!(
        reason.ends_with("only you can\n1. Add the key\n2. Press Done"),
        "{reason}"
    );
}

#[tokio::test]
async fn a_record_that_already_holds_a_finding_is_not_looked_at_again() {
    let (mut rig, fake) = stopped(vec![found("x")]).await;
    rig.machine.pass().await;
    assert_eq!(looks(&fake), 1);
    // A restart keeps the record: even without its mark, the reason says Claude looked.
    rig.lp(0).records.get_mut("a").unwrap().triaged = false;
    rig.lp(0).triaging.clear();
    rig.machine.pass().await;
    assert_eq!(looks(&fake), 1);
}

#[tokio::test]
async fn triage_off_never_looks() {
    let (mut rig, fake, _asked) = settling_ended(SessionResult::NeedsHuman("first problem".into())).await;
    rig.lp(0).project.triage = OnOff::Off;
    for _ in 0..3 {
        rig.machine.pass().await;
    }
    assert_eq!(looks(&fake), 0);
}

#[tokio::test]
async fn a_paused_project_does_not_look() {
    let (mut rig, fake, _asked) = settling_ended(SessionResult::NeedsHuman("first problem".into())).await;
    rig.pause("p0", true);
    for _ in 0..3 {
        rig.machine.pass().await;
    }
    assert_eq!(looks(&fake), 0);
}

#[tokio::test]
async fn a_failing_look_retries_after_ten_then_twenty_minutes_and_gives_up_with_a_note_on_the_third() {
    let ends = vec![failed(LookFailure::Other); 3];
    let (mut rig, fake) = stopped(ends).await;
    rig.machine.pass().await;
    assert_eq!(looks(&fake), 1);
    let wait = |rig: &mut Rig| {
        let at = rig.lp(0).records["a"].next_look_at.clone().unwrap();
        let at = chrono::DateTime::parse_from_rfc3339(&at).unwrap();
        (at.with_timezone(&chrono::Utc) - chrono::Utc::now()).num_minutes()
    };
    assert_eq!((rig.lp(0).records["a"].failed_looks, wait(&mut rig)), (1, 9));
    rig.machine.pass().await;
    assert_eq!(looks(&fake), 1, "not before the wait is over");
    past(&mut rig);
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(looks(&fake), 2);
    assert_eq!((rig.lp(0).records["a"].failed_looks, wait(&mut rig)), (2, 19));
    past(&mut rig);
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(looks(&fake), 3);
    let r = rig.lp(0).records["a"].clone();
    assert!(r.triaged && r.failed_looks == 3);
    let reason = r.needs_human_reason.unwrap();
    assert!(reason.contains("Claude couldn't look into it"), "{reason}");
    for _ in 0..3 {
        past(&mut rig);
        rig.machine.pass().await;
    }
    assert_eq!(looks(&fake), 3, "not looked at again for this stop");
}

#[tokio::test]
async fn a_look_that_hit_its_budget_is_not_retried() {
    let (mut rig, fake) = stopped(vec![failed(LookFailure::BudgetHit)]).await;
    for _ in 0..3 {
        rig.machine.pass().await;
    }
    assert_eq!(looks(&fake), 1);
    let r = rig.lp(0).records["a"].clone();
    assert!(r.triaged && r.failed_looks == 0);
}

#[tokio::test]
async fn a_new_stop_is_looked_at_again() {
    let (mut rig, fake) = stopped(vec![found("one"), found("two")]).await;
    rig.machine.pass().await;
    assert_eq!(looks(&fake), 1);
    let slice = rig.lp(0).records["a"].slice;
    rig.lp(0)
        .records
        .get_mut("a")
        .unwrap()
        .new_stop("reviewing", "later failure", slice);
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert_eq!(looks(&fake), 2);
    let reason = rig.lp(0).records["a"].needs_human_reason.clone().unwrap();
    assert!(
        reason.starts_with("later failure") && reason.contains("two") && !reason.contains("one"),
        "{reason}"
    );
}

#[tokio::test]
async fn claude_is_on_the_item_while_the_look_runs_and_not_after() {
    let (mut rig, fake) = stopped(vec![]).await;
    assert_eq!(looks(&fake), 1);
    assert!(rig.lp(0).claude_on_it("a"));
    assert_eq!(on_it_events(&mut rig).last(), Some(&Some(true)));
    rig.machine.outcome_sender().send(SessionOutcome {
        project: "p0".into(),
        item_id: "a".into(),
        lane: 0,
        result: SessionResult::Triage(found("done")),
        context_tokens: None,
        usage: None,
        notes: vec![],
        session_id: None,
    });
    rig.machine.pass().await;
    assert!(!rig.lp(0).claude_on_it("a"));
    assert_eq!(on_it_events(&mut rig).last(), Some(&Some(false)));
}

#[tokio::test]
async fn with_the_triage_session_flag_off_nothing_looks_and_no_event_carries_claude_on_it() {
    let (mut rig, fake, _asked) = settling_ended(SessionResult::NeedsHuman("first problem".into())).await;
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&[]));
    rig.lp(0).triage_flag = false;
    for _ in 0..3 {
        rig.machine.pass().await;
    }
    assert_eq!(looks(&fake), 0);
    assert!(on_it_events(&mut rig).is_empty());
}

fn decision(decision: TriageDecision, instructions: &str) -> TriageEnd {
    TriageEnd::Found {
        decision,
        reason: "because".into(),
        steps: vec![],
        instructions: instructions.into(),
    }
}

/// `stopped`, with a branch and a session on the record to carry on from.
async fn decided(end: TriageEnd) -> (Rig, Arc<Fake>) {
    let (mut rig, fake) = stopped(vec![end]).await;
    let record = rig.lp(0).records.get_mut("a").unwrap();
    record.branch = Some("a".into());
    record.impl_session_id = Some("s1".into());
    rig.machine.pass().await;
    (rig, fake)
}

fn notes(rig: &mut Rig) -> String {
    rig.lp(0).item_notes("a", 20).join("\n")
}

#[tokio::test]
async fn a_continue_decision_resumes_the_item_with_the_instructions_and_counts() {
    let (mut rig, fake) = decided(decision(TriageDecision::Continue, "Use the other API.")).await;
    let started = fake.started.lock().unwrap().clone()[1..].to_vec();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].follow_up.as_deref(), Some("Use the other API."));
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    let r = &rig.lp(0).records["a"];
    assert_eq!(r.triages, Some((r.slice, 1)));
    assert!(notes(&mut rig).contains("Claude decided to continue: because"));
}

#[tokio::test]
async fn a_start_over_decision_starts_a_new_attempt_with_the_instructions_as_notes() {
    let (_rig, fake) = decided(decision(TriageDecision::StartOver, "Try the simple way.")).await;
    let started = fake.started.lock().unwrap().clone()[1..].to_vec();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].notes.as_deref(), Some("Try the simple way."));
    assert_eq!(started[0].attempt, 1);
    assert_eq!(fake.abandoned.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_skip_decision_skips_the_item() {
    let (mut rig, fake) = decided(decision(TriageDecision::Skip, "")).await;
    assert_eq!(fake.started.lock().unwrap().len(), 1, "only the first implementer");
    assert!(!rig.lp(0).runs.contains_key("a"));
    assert!(notes(&mut rig).contains("Claude decided to skip: because"));
}

#[tokio::test]
async fn a_third_automatic_decision_in_a_slice_is_left_for_a_person() {
    let (mut rig, fake) = stopped(vec![decision(TriageDecision::Continue, "go")]).await;
    let slice = rig.lp(0).records["a"].slice;
    rig.lp(0).records.get_mut("a").unwrap().triages = Some((slice, 2));
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1, "only the first implementer");
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    assert!(
        rig.lp(0).records["a"]
            .needs_human_reason
            .clone()
            .unwrap()
            .contains("Claude looked into it")
    );
}

#[tokio::test]
async fn a_decision_waiting_for_a_lane_starts_when_one_frees_without_another_look() {
    let (mut rig, fake) = stopped(vec![decision(TriageDecision::Continue, "go")]).await;
    let holders: Vec<_> = (1..=2)
        .map(|n| Holder::Item {
            project: "p9".into(),
            item: format!("other{n}"),
        })
        .collect();
    for (n, h) in holders.iter().enumerate() {
        rig.machine.pool.take(n as u32 + 1, h.clone());
    }
    let record = rig.lp(0).records.get_mut("a").unwrap();
    record.branch = Some("a".into());
    record.impl_session_id = Some("s1".into());
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1, "only the first implementer");
    assert!(rig.lp(0).claude_on_it("a"), "still Claude's while it waits");
    let looked = looks(&fake);
    for (n, h) in holders.iter().enumerate() {
        rig.machine.pool.release(n as u32 + 1, h);
    }
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 2);
    assert_eq!(looks(&fake), looked, "no second look");
}

#[tokio::test]
async fn a_persons_command_drops_claudes_waiting_decision() {
    let (mut rig, fake) = stopped(vec![decision(TriageDecision::Continue, "Claude's way")]).await;
    let holder = Holder::Item {
        project: "p9".into(),
        item: "other".into(),
    };
    for lane in 1..=2 {
        rig.machine.pool.take(lane, holder.clone());
    }
    let record = rig.lp(0).records.get_mut("a").unwrap();
    record.branch = Some("a".into());
    record.impl_session_id = Some("s1".into());
    rig.machine.pass().await;
    rig.machine
        .links
        .item_commands
        .push(crate::view::ItemCommand::Continue {
            project: "p0".into(),
            item: "a".into(),
            notes: Some("Person's way".into()),
        });
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).continuing.len(), 1);
    for lane in 1..=2 {
        rig.machine.pool.release(lane, &holder);
    }
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap().clone()[1..].to_vec();
    assert_eq!(started.len(), 1);
    assert_eq!(started[0].follow_up.as_deref(), Some("Person's way"));
}

#[tokio::test]
async fn person_steps_with_no_pr_waits_for_the_steps_and_with_a_pr_is_human() {
    let end = |steps: &[&str]| TriageEnd::Found {
        decision: TriageDecision::PersonSteps,
        reason: "only you can".into(),
        steps: steps.iter().map(|s| s.to_string()).collect(),
        instructions: String::new(),
    };
    let (mut rig, _fake) = stopped(vec![end(&["Add the key"])]).await;
    rig.machine.pass().await;
    let r = &rig.lp(0).records["a"];
    assert_eq!(
        r.needs_human_reason.as_deref(),
        Some("Only steps a person can do are left:\n1. Add the key")
    );
    assert!(r.triaged);
    assert!(notes(&mut rig).contains("Claude decided to wait for your steps: only you can"));

    let (mut rig, _fake) = stopped(vec![end(&["Add the key"])]).await;
    rig.lp(0).items.get_mut("a").unwrap().pr_url = Some("https://github.com/x/y/pull/1".into());
    rig.machine.pass().await;
    let reason = rig.lp(0).records["a"].needs_human_reason.clone().unwrap();
    assert!(reason.ends_with("only you can\n1. Add the key"), "{reason}");
}
