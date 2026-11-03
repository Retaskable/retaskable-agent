//! A stopped item gets one look from a triager (`driver/triage_run.rs`, 06.9 bullets 16 and 20).
use super::*;
use crate::config::OnOff;
use crate::connector::bug_link::BugLink;
use crate::triage::{RetaskableBug, TriageDecision};

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
async fn person_steps_are_left_as_a_human_finding_with_the_steps_listed_under_the_reason() {
    let end = TriageEnd::Found {
        decision: TriageDecision::PersonSteps,
        reason: "only you can".into(),
        steps: vec!["Add the key".into(), "Press Done".into()],
        bug: None,
    };
    let (mut rig, _fake) = stopped(vec![end]).await;
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

/// A link to a fake hub: consent `on`, every `bug_report` it is sent answered as `answer` says; the sent ones listed.
fn fake_hub(on: bool, answer: Value) -> (Arc<BugLink>, Arc<Mutex<Vec<Value>>>) {
    let link = Arc::new(BugLink::new(std::time::Duration::from_secs(5)));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    link.opened(tx);
    link.consent_said(&json!({ "on": on, "known": [{ "id": "b1", "title": "Known" }] }));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let (l, s) = (link.clone(), sent.clone());
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let mut reply = answer.clone();
            reply["ref"] = msg["ref"].clone();
            s.lock().unwrap().push(msg);
            l.answered(&reply);
        }
    });
    (link, sent)
}

fn found_with_bug(decision: TriageDecision) -> TriageEnd {
    TriageEnd::Found {
        decision,
        reason: "r".into(),
        steps: vec![],
        bug: Some(RetaskableBug {
            title: "A prompt left {{name}} unfilled".into(),
            details: "details".into(),
            same_as: None,
        }),
    }
}

fn send_end(rig: &mut Rig, end: TriageEnd) {
    rig.machine.outcome_sender().send(SessionOutcome {
        project: "p0".into(),
        item_id: "a".into(),
        lane: 0,
        result: SessionResult::Triage(end),
        context_tokens: None,
        usage: None,
        notes: vec![],
        session_id: None,
    });
}

/// Passes until `sent` holds `n` reports and the timeline has had `lines` of them answered.
async fn settle_bug(rig: &mut Rig, lines: usize) -> Vec<String> {
    for _ in 0..50 {
        rig.machine.pass().await;
        let notes: Vec<String> = (rig.lp(0).item_notes("a", 20).into_iter())
            .filter(|n| n.contains("Retaskable"))
            .collect();
        if notes.len() >= lines {
            return notes;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    Vec::new()
}

#[tokio::test]
async fn a_bug_with_consent_is_sent_once_with_its_timeline_line_and_never_again_even_after_a_restart() {
    let (mut rig, _fake) = stopped(vec![]).await;
    let (link, sent) = fake_hub(true, json!({ "type": "bug_reported", "ok": true }));
    rig.lp(0).bugs = Some((link, "1.0 (abc)".into()));
    send_end(&mut rig, found_with_bug(TriageDecision::Continue));
    let notes = settle_bug(&mut rig, 1).await;
    assert_eq!(notes, vec!["Reported to Retaskable as a fix"]);
    let msg = sent.lock().unwrap()[0].clone();
    assert_eq!(msg["type"], "bug_report");
    assert_eq!(msg["version"], "1.0 (abc)");
    let signature = rig.lp(0).records["a"].reported_bugs.clone();
    assert_eq!(signature, vec![msg["signature"].as_str().unwrap().to_string()]);
    // A restart keeps the record; a look at the same stop that names the same bug sends nothing.
    rig.lp(0).records.get_mut("a").unwrap().needs_human_reason = Some("first problem".into());
    send_end(&mut rig, found_with_bug(TriageDecision::Human));
    for _ in 0..5 {
        rig.machine.pass().await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(sent.lock().unwrap().len(), 1);
    assert_eq!(rig.lp(0).records["a"].reported_bugs, signature);
}

#[tokio::test]
async fn with_consent_off_nothing_is_sent_and_no_timeline_line_is_added() {
    let (mut rig, _fake) = stopped(vec![]).await;
    let (link, sent) = fake_hub(false, json!({ "ok": true }));
    rig.lp(0).bugs = Some((link, "1.0".into()));
    send_end(&mut rig, found_with_bug(TriageDecision::Human));
    for _ in 0..5 {
        rig.machine.pass().await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(sent.lock().unwrap().is_empty());
    assert!(rig.lp(0).records["a"].reported_bugs.is_empty());
    assert!(rig.lp(0).item_notes("a", 20).iter().all(|n| !n.contains("Reported to")));
}

#[tokio::test]
async fn a_duplicate_answer_adds_the_duplicate_line() {
    let (mut rig, _fake) = stopped(vec![]).await;
    let (link, _sent) = fake_hub(true, json!({ "ok": true, "duplicate": true, "itemId": "b1" }));
    rig.lp(0).bugs = Some((link, "1.0".into()));
    send_end(&mut rig, found_with_bug(TriageDecision::Human));
    let notes = settle_bug(&mut rig, 1).await;
    assert_eq!(notes, vec!["Retaskable already has this bug on its board"]);
}

#[tokio::test]
async fn the_decision_is_carried_out_the_same_with_or_without_the_bug() {
    let reason_of = |bug: bool| async move {
        let (mut rig, _fake) = stopped(vec![]).await;
        let (link, _sent) = fake_hub(true, json!({ "ok": true }));
        rig.lp(0).bugs = Some((link, "1.0".into()));
        let mut end = found_with_bug(TriageDecision::Human);
        if let TriageEnd::Found { bug: b, .. } = &mut end
            && !bug
        {
            *b = None;
        }
        send_end(&mut rig, end);
        rig.machine.pass().await;
        let r = rig.lp(0).records["a"].clone();
        (r.needs_human_reason, r.triaged)
    };
    assert_eq!(reason_of(true).await, reason_of(false).await);
}
