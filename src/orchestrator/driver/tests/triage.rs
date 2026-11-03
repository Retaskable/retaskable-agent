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

/// A fake hub connection: consent `on`, answering each `bug_report` with `duplicate` (or not answering when
/// `answers` is false). Returns the link and the `bug_report`s it was sent.
fn fake_link(
    on: bool,
    answers: bool,
    duplicate: bool,
) -> (Arc<crate::connector::bug_link::BugLink>, Arc<Mutex<Vec<Value>>>) {
    let link = Arc::new(crate::connector::bug_link::BugLink::new(Duration::from_millis(300)));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    link.opened(tx);
    link.consent_said(&json!({ "type": "bug_reports", "on": on, "known": [{ "id": "b1", "title": "Known" }] }));
    let sent = Arc::new(Mutex::new(Vec::new()));
    let (l, s) = (link.clone(), sent.clone());
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if answers {
                l.answered(&json!({ "type": "bug_reported", "ref": msg["ref"], "ok": true, "duplicate": duplicate }));
            }
            s.lock().unwrap().push(msg);
        }
    });
    (link, sent)
}

fn found_with_bug(decision: TriageDecision) -> TriageEnd {
    TriageEnd::Found {
        decision,
        reason: "r".into(),
        steps: vec![],
        bug: Some(crate::triage::RetaskableBug {
            title: "A prompt variable wasn't filled".into(),
            details: "details".into(),
            same_as: Some("b1".into()),
        }),
    }
}

/// "a" stopped, the link in place, and the finding read, with a few passes for the answer to come back.
async fn reported(link: Arc<crate::connector::bug_link::BugLink>, ends: Vec<TriageEnd>) -> (Rig, Arc<Fake>) {
    let (mut rig, fake) = stopped(ends).await;
    rig.lp(0).bugs = Some((link, "1.2.3".into()));
    for _ in 0..3 {
        rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    (rig, fake)
}

fn timeline(rig: &mut Rig) -> Vec<String> {
    rig.lp(0).item_notes("a", 50)
}

#[tokio::test]
async fn a_bug_with_consent_on_is_sent_once_with_its_timeline_line_and_the_same_signature_never_again() {
    let (link, sent) = fake_link(true, true, false);
    let (mut rig, fake) = reported(link.clone(), vec![found_with_bug(TriageDecision::Human)]).await;
    assert_eq!(sent.lock().unwrap().len(), 1);
    let msg = sent.lock().unwrap()[0].clone();
    assert_eq!(msg["type"], "bug_report");
    assert_eq!(msg["sameAs"], "b1");
    assert_eq!(msg["version"], "1.2.3");
    assert!(
        timeline(&mut rig)
            .iter()
            .any(|n| n == "Reported to Retaskable as a fix")
    );
    // The signature is in the saved state, so a restart still has it.
    let saved = Store::load(&rig._dir.path().join("0")).unwrap().runs();
    assert_eq!(saved["a"].reported_bugs.len(), 1);
    // A later look at the same item naming the same bug sends nothing.
    // A new stop of the same item (the signature stays: it is the item's).
    rig.lp(0)
        .records
        .get_mut("a")
        .unwrap()
        .new_stop("checking", "first problem", 1);
    *fake.triage.ends.lock().unwrap() = vec![found_with_bug(TriageDecision::Human)];
    for _ in 0..3 {
        rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(looks(&fake), 2);
    assert_eq!(sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn consent_off_sends_nothing_and_adds_no_timeline_line() {
    let (link, sent) = fake_link(false, true, false);
    let (mut rig, _fake) = reported(link, vec![found_with_bug(TriageDecision::Human)]).await;
    assert!(sent.lock().unwrap().is_empty());
    assert!(rig.lp(0).records["a"].reported_bugs.is_empty());
    assert!(!timeline(&mut rig).iter().any(|n| n.contains("Retaskable")));
}

#[tokio::test]
async fn a_duplicate_answer_adds_the_duplicate_line() {
    let (link, _sent) = fake_link(true, true, true);
    let (mut rig, _fake) = reported(link, vec![found_with_bug(TriageDecision::Human)]).await;
    assert!(
        timeline(&mut rig)
            .iter()
            .any(|n| n == "Retaskable already has this bug on its board")
    );
}

#[tokio::test]
async fn an_unanswered_bug_is_kept_as_sent_and_adds_no_line() {
    let (link, sent) = fake_link(true, false, false);
    let (mut rig, _fake) = reported(link, vec![found_with_bug(TriageDecision::Human)]).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    rig.machine.pass().await;
    assert_eq!(sent.lock().unwrap().len(), 1);
    assert_eq!(rig.lp(0).records["a"].reported_bugs.len(), 1);
    assert!(!timeline(&mut rig).iter().any(|n| n.contains("Retaskable")));
}

#[tokio::test]
async fn the_decision_is_carried_out_the_same_with_or_without_the_bug() {
    let (link, _sent) = fake_link(true, true, false);
    let (mut with, _f) = reported(link, vec![found_with_bug(TriageDecision::PersonSteps)]).await;
    let (mut without, _g) = reported(
        fake_link(true, true, false).0,
        vec![TriageEnd::Found {
            decision: TriageDecision::PersonSteps,
            reason: "r".into(),
            steps: vec![],
            bug: None,
        }],
    )
    .await;
    assert_eq!(with.lp(0).records["a"].state, without.lp(0).records["a"].state);
    assert_eq!(
        with.lp(0).records["a"].needs_human_reason,
        without.lp(0).records["a"].needs_human_reason
    );
}

#[tokio::test]
async fn the_hubs_known_bugs_are_given_to_the_triager_when_the_look_starts() {
    let (base, _asked) = claim_stub_with(json!([item("a", "claimed")]), handed_out(), Default::default()).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["triage-session"]));
    rig.lp(0).bugs = Some((fake_link(true, true, false).0, "v".into()));
    let fake = rig.go_live();
    fake.script
        .lock()
        .unwrap()
        .push(SessionResult::NeedsHuman("first problem".into()));
    rig.machine.pass().await;
    rig.machine.pass().await;
    let asked = fake.triage.asked.lock().unwrap();
    assert_eq!(asked[0].context.known_bugs[0].id, "b1");
}
