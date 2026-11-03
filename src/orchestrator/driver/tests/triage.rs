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
        bug: None,
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
        bug: None,
    }
}

/// "a" has a branch and a session to carry on from, and is on slice 1 of the store's view.
fn pushed(rig: &mut Rig) {
    let r = rig.lp(0).records.get_mut("a").unwrap();
    r.branch = Some("a".into());
    r.impl_session_id = Some("s1".into());
}

fn timeline(rig: &mut Rig) -> Vec<String> {
    rig.lp(0).item_notes("a", 20)
}

#[tokio::test]
async fn a_continue_decision_resumes_the_item_on_its_branch_with_the_instructions_and_counts() {
    let (mut rig, fake) = stopped(vec![decision(TriageDecision::Continue, "Use the other API.")]).await;
    pushed(&mut rig);
    let before = fake.started.lock().unwrap().len();
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap();
    assert_eq!(started.len(), before + 1);
    assert_eq!(started.last().unwrap().follow_up.as_deref(), Some("Use the other API."));
    drop(started);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::Implementing);
    let r = &rig.lp(0).records["a"];
    assert_eq!(r.triages, Some((r.slice, 1)));
    assert!(
        timeline(&mut rig)
            .iter()
            .any(|n| n == "Claude decided to continue: because")
    );
    assert!(rig.lp(0).claude_pending.is_empty());
}

#[tokio::test]
async fn a_start_over_decision_starts_a_new_attempt_with_the_instructions_in_its_first_prompt() {
    let (mut rig, fake) = stopped(vec![decision(TriageDecision::StartOver, "Try a smaller change.")]).await;
    pushed(&mut rig);
    let before = fake.started.lock().unwrap().len();
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap();
    assert_eq!(started.len(), before + 1);
    let last = started.last().unwrap();
    assert_eq!(
        (last.attempt, last.notes.as_deref()),
        (1, Some("Try a smaller change."))
    );
    drop(started);
    assert_eq!(rig.lp(0).records["a"].triages.map(|t| t.1), Some(1));
}

#[tokio::test]
async fn a_skip_decision_skips_the_item_in_the_store() {
    let (base, asked) = claim_stub_with(json!([item("a", "claimed")]), handed_out(), Default::default()).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["triage-session"]));
    let fake = rig.go_live();
    fake.script
        .lock()
        .unwrap()
        .push(SessionResult::NeedsHuman("first problem".into()));
    rig.machine.pass().await;
    *fake.triage.ends.lock().unwrap() = vec![decision(TriageDecision::Skip, "")];
    rig.machine.pass().await;
    rig.machine.pass().await;
    assert!(!rig.lp(0).runs.contains_key("a"));
    assert!(ops(&asked).contains(&"skip".to_string()), "{:?}", ops(&asked));
}

#[tokio::test]
async fn a_decision_past_the_cap_is_left_for_a_person() {
    let (mut rig, fake) = stopped(vec![]).await;
    pushed(&mut rig);
    let slice = rig.lp(0).records["a"].slice;
    rig.lp(0).records.get_mut("a").unwrap().triages = Some((slice, 2));
    rig.machine.outcome_sender().send(SessionOutcome {
        project: "p0".into(),
        item_id: "a".into(),
        lane: 0,
        result: SessionResult::Triage(decision(TriageDecision::Continue, "again")),
        context_tokens: None,
        usage: None,
        notes: vec![],
        session_id: None,
    });
    let before = fake.started.lock().unwrap().len();
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), before);
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
async fn a_decision_waiting_for_a_lane_starts_when_one_frees_with_no_second_look() {
    let (mut rig, fake) = stopped(vec![decision(TriageDecision::Continue, "Go on.")]).await;
    pushed(&mut rig);
    let holders: Vec<_> = (1..=2)
        .map(|n| Holder::Item {
            project: "p9".into(),
            item: format!("o{n}"),
        })
        .collect();
    for (n, h) in holders.iter().enumerate() {
        rig.machine.pool.take(n as u32 + 1, h.clone());
    }
    let before = fake.started.lock().unwrap().len();
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), before);
    assert!(rig.lp(0).claude_on_it("a"), "claudeOnIt stays true while it waits");
    rig.machine.pass().await;
    assert_eq!(looks(&fake), 1);
    rig.machine.pool.release(1, &holders[0]);
    rig.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), before + 1);
    assert_eq!(looks(&fake), 1, "Claude isn't asked again");
    assert!(!rig.lp(0).claude_on_it("a"));
}

#[tokio::test]
async fn a_persons_command_drops_claudes_waiting_decision() {
    let (mut rig, fake) = stopped(vec![decision(TriageDecision::Continue, "Claude's way.")]).await;
    pushed(&mut rig);
    let holders: Vec<_> = (1..=2)
        .map(|n| Holder::Item {
            project: "p9".into(),
            item: format!("o{n}"),
        })
        .collect();
    for (n, h) in holders.iter().enumerate() {
        rig.machine.pool.take(n as u32 + 1, h.clone());
    }
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).continuing.len(), 1);
    rig.machine
        .links
        .item_commands
        .push(crate::view::ItemCommand::Continue {
            project: "p0".into(),
            item: "a".into(),
            notes: Some("Person's way.".into()),
        });
    rig.machine.pool.release(1, &holders[0]);
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap();
    assert_eq!(started.last().unwrap().follow_up.as_deref(), Some("Person's way."));
    drop(started);
    assert!(rig.lp(0).continuing.is_empty() && rig.lp(0).claude_pending.is_empty());
}

fn steps_end() -> TriageEnd {
    TriageEnd::Found {
        decision: TriageDecision::PersonSteps,
        reason: "only you can".into(),
        steps: vec!["Add the key".into(), "Press Done".into()],
        instructions: String::new(),
        bug: None,
    }
}

#[tokio::test]
async fn person_steps_with_no_pr_wait_for_the_steps() {
    let (mut rig, _fake) = stopped(vec![steps_end()]).await;
    rig.machine.pass().await;
    let r = rig.lp(0).records["a"].clone();
    assert_eq!(
        r.needs_human_reason.unwrap(),
        "Only steps a person can do are left:\n1. Add the key\n2. Press Done"
    );
    assert!(r.triaged && r.triages.is_none());
    assert!(!rig.lp(0).claude_on_it("a"));
}

#[tokio::test]
async fn person_steps_with_a_pr_are_human_with_the_steps_under_the_reason() {
    let (mut rig, _fake) = stopped(vec![steps_end()]).await;
    rig.lp(0).items.get_mut("a").unwrap().pr_url = Some("https://github.com/o/p/pull/3".into());
    rig.machine.pass().await;
    let reason = rig.lp(0).records["a"].needs_human_reason.clone().unwrap();
    assert!(
        reason.starts_with("first problem") && reason.ends_with("1. Add the key\n2. Press Done"),
        "{reason}"
    );
}

// Reporting the triager's Retaskable bugs (06.9 bullet 19), with a fake hub behind a real `BugLink`.

fn bug_end(decision: TriageDecision) -> TriageEnd {
    TriageEnd::Found {
        decision,
        reason: "because".into(),
        steps: vec![],
        instructions: String::new(),
        bug: Some(crate::triage::RetaskableBug {
            title: "Lane prep drops the sync".into(),
            details: "The sync step is skipped.".into(),
            same_as: None,
        }),
    }
}

/// The project's hub link, consent `on`, answering each `bug_report` with `answer`; the reports it got.
fn hub(rig: &mut Rig, on: bool, answer: Value) -> Arc<Mutex<Vec<Value>>> {
    use crate::connector::bug_link::BugLink;
    let link = Arc::new(BugLink::new(Duration::from_secs(5)));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    link.opened(tx);
    link.consent_said(&json!({ "type": "bug_reports", "on": on, "known": [] }));
    let got = Arc::new(Mutex::new(Vec::new()));
    let (seen, answering) = (got.clone(), link.clone());
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let mut reply = answer.clone();
            reply["ref"] = msg["ref"].clone();
            seen.lock().unwrap().push(msg);
            answering.answered(&reply);
        }
    });
    rig.machine.links.bugs.insert("p0".into(), link);
    got
}

async fn settle_bug(rig: &mut Rig) {
    rig.machine.pass().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    rig.machine.pass().await;
}

fn has_note(rig: &mut Rig, text: &str) -> bool {
    timeline(rig).iter().any(|n| n == text)
}

#[tokio::test]
async fn a_bug_with_consent_on_is_sent_once_and_its_timeline_line_added() {
    let (mut rig, _fake) = stopped(vec![bug_end(TriageDecision::Human), bug_end(TriageDecision::Human)]).await;
    let got = hub(&mut rig, true, json!({ "type": "bug_reported", "ok": true }));
    settle_bug(&mut rig).await;
    assert_eq!(got.lock().unwrap().len(), 1);
    assert_eq!(got.lock().unwrap()[0]["title"], "Lane prep drops the sync");
    assert!(has_note(&mut rig, "Reported to Retaskable as a fix"));
    let sig = rig.lp(0).store.runs()["a"].reported_bugs.clone();
    assert_eq!(sig.len(), 1);
    // A second look finds the same bug: never sent again for the item.
    let r = rig.lp(0).records.get_mut("a").unwrap();
    r.triaged = false;
    rig.lp(0).triaging.clear();
    past(&mut rig);
    settle_bug(&mut rig).await;
    assert_eq!(got.lock().unwrap().len(), 1);
    assert_eq!(rig.lp(0).records["a"].reported_bugs, sig);
}

#[tokio::test]
async fn consent_off_sends_nothing_and_adds_no_line() {
    let (mut rig, _fake) = stopped(vec![bug_end(TriageDecision::Human)]).await;
    let got = hub(&mut rig, false, json!({ "type": "bug_reported", "ok": true }));
    settle_bug(&mut rig).await;
    assert!(got.lock().unwrap().is_empty());
    assert!(rig.lp(0).records["a"].reported_bugs.is_empty());
    assert!(!has_note(&mut rig, "Reported to Retaskable as a fix"));
}

#[tokio::test]
async fn a_duplicate_answer_adds_the_duplicate_line() {
    let (mut rig, _fake) = stopped(vec![bug_end(TriageDecision::Human)]).await;
    let answer = json!({ "type": "bug_reported", "ok": true, "duplicate": true, "itemId": "b1" });
    let got = hub(&mut rig, true, answer);
    settle_bug(&mut rig).await;
    assert_eq!(got.lock().unwrap().len(), 1);
    assert!(has_note(&mut rig, "Retaskable already has this bug on its board"));
}

#[tokio::test]
async fn a_refused_report_is_marked_but_adds_no_line() {
    let (mut rig, _fake) = stopped(vec![bug_end(TriageDecision::Human)]).await;
    let got = hub(
        &mut rig,
        true,
        json!({ "type": "bug_reported", "ok": false, "reason": "no" }),
    );
    settle_bug(&mut rig).await;
    assert_eq!(got.lock().unwrap().len(), 1);
    assert_eq!(rig.lp(0).records["a"].reported_bugs.len(), 1);
    assert!(!has_note(&mut rig, "Reported to Retaskable as a fix"));
}

#[tokio::test]
async fn the_decision_is_carried_out_the_same_with_the_bug() {
    let (mut rig, _fake) = stopped(vec![bug_end(TriageDecision::Skip)]).await;
    let got = hub(&mut rig, true, json!({ "type": "bug_reported", "ok": true }));
    settle_bug(&mut rig).await;
    assert_eq!(got.lock().unwrap().len(), 1);
    assert!(has_note(&mut rig, "Claude decided to skip: because"));
}
