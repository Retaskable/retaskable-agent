//! A completion the store hasn't taken (`driver/outbox.rs`): listed in the state file before it is sent, sent after the
//! item's decisions, kept while the store can't answer, sent again after a restart, and dropped for a definite refusal.
use super::asks::{Listed, ops_named, push, store_failing, until};
use super::restart::restarted;
use super::*;
use crate::backlog::complete::{Completion, Pending};
use crate::orchestrator::driver::finish_steps::FinishHost;

const DAY: i64 = 24 * 60 * 60;

struct Head;

impl FinishHost for Head {
    fn base_head(
        &self,
        _: std::path::PathBuf,
        _: String,
        _: String,
    ) -> futures_util::future::BoxFuture<'static, anyhow::Result<(String, bool)>> {
        Box::pin(async { Ok(("abc123".to_string(), true)) })
    }
}

fn refusal(status: u16, code: &str) -> (u16, Value) {
    (status, json!({ "error": code }))
}

fn pending(since: i64) -> Pending {
    Pending {
        lease: "lease-1".into(),
        completion: Completion {
            built: false,
            remaining: Some("the page".into()),
            ..Default::default()
        },
        since,
        completed: false,
        tryout: None,
    }
}

/// A computer holding nothing, with a completion for item 4 listed (and saved), and the store's answers to `complete`.
async fn rig_owing(answers: Vec<(u16, Value)>) -> (Rig, Asked, Listed, String) {
    let (base, asked, listed) = store_failing("").await;
    *listed.complete_answers.lock().unwrap() = answers;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    *listed.state_file.lock().unwrap() = Some(rig._dir.path().join("0").join("state.json"));
    rig.lp(0).clock = Some(1000);
    rig.lp(0).outbox.insert("4".into(), pending(1000));
    rig.lp(0).save_leases();
    (rig, asked, listed, base)
}

fn on_disk(rig: &mut Rig) -> Vec<String> {
    let reloaded = Store::load(&rig._dir.path().join("0")).unwrap();
    reloaded.complete_outbox().into_keys().collect()
}

fn ops(asked: &Asked, names: &[&str]) -> Vec<String> {
    (asked.lock().unwrap().iter())
        .map(|(op, _)| op.clone())
        .filter(|op| names.contains(&op.as_str()))
        .collect()
}

fn errors(rig: &mut Rig, kind: &str) -> usize {
    rig.lp(0)
        .events()
        .recent()
        .iter()
        .map(crate::telemetry::Entry::to_value)
        .filter(|e| e["kind"] == kind && (e["item"] == "4" || e["op"] == "complete"))
        .count()
}

/// Sends item 4's listed call as a finish or a merge does, without a pass in between that might move the held lease.
async fn send(rig: &mut Rig) {
    let done = pending(0).completion;
    rig.lp(0).complete_item(7, "tok", "4", "lease-1", done).await;
}

#[tokio::test]
async fn the_items_decisions_are_sent_before_its_completion() {
    let (mut rig, asked, _listed, _) = rig_owing(vec![]).await;
    let mut held = crate::backlog::decisions::sync::ItemDecisions::default();
    held.now.new_stop("implementing", 1);
    rig.lp(0).decisions.insert("4".into(), held);
    send(&mut rig).await;
    assert_eq!(
        ops(&asked, &["details_report", "complete"]),
        ["details_report", "complete"]
    );
    assert!(rig.lp(0).outbox.is_empty());
}

#[tokio::test]
async fn a_decisions_flush_the_store_refuses_keeps_the_completion_listed_and_it_goes_later() {
    let (base, asked, _listed) = store_failing("details_report").await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.lp(0).clock = Some(0);
    rig.lp(0).outbox.insert("4".into(), pending(0));
    let mut held = crate::backlog::decisions::sync::ItemDecisions::default();
    held.now.new_stop("implementing", 1);
    rig.lp(0).decisions.insert("4".into(), held);
    send(&mut rig).await;
    assert!(ops_named(&asked, "complete").is_empty(), "the decisions go first");
    assert!(rig.lp(0).outbox.contains_key("4"));
}

#[tokio::test]
async fn a_completion_the_store_cant_take_is_on_disk_before_it_is_sent_and_sent_again_next_pass() {
    let (mut rig, asked, listed, _) = rig_owing(vec![refusal(500, "boom")]).await;
    rig.lp(0).outbox.clear();
    let done = Completion {
        built: false,
        remaining: Some("x".into()),
        ..Default::default()
    };
    let sent = rig.lp(0).complete_item(7, "tok", "4", "lease-1", done).await;
    assert!(matches!(sent, super::super::outbox::Sent::Kept(_)));
    assert_eq!(on_disk(&mut rig), ["4"], "kept after the store said no");
    assert_eq!(
        *listed.listed_on_disk.lock().unwrap(),
        [true],
        "listed before it was sent"
    );
    rig.machine.pass().await;
    assert_eq!(ops_named(&asked, "complete").len(), 2);
    assert!(rig.lp(0).outbox.is_empty());
    assert!(on_disk(&mut rig).is_empty());
}

#[tokio::test]
async fn an_unreachable_store_keeps_the_entry_until_it_answers() {
    let (mut rig, asked, _listed, _) = rig_owing(vec![]).await;
    // A store at an address nothing listens on.
    let gone = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = gone.local_addr().unwrap();
    drop(gone);
    rig.lp(0).client = Client::new(format!("http://{addr}")).into();
    rig.machine.pass().await;
    assert_eq!(on_disk(&mut rig), ["4"]);
    assert!(ops_named(&asked, "complete").is_empty());
}

#[tokio::test]
async fn a_restart_sends_what_was_listed() {
    let (rig, asked, listed, base) = rig_owing(vec![refusal(500, "boom")]).await;
    let mut rig = rig;
    rig.machine.pass().await;
    assert_eq!(ops_named(&asked, "complete").len(), 1);
    let mut rig = restarted(rig, &base);
    rig.go_live();
    assert_eq!(rig.lp(0).outbox["4"].lease, "lease-1");
    rig.machine.pass().await;
    let sent = ops_named(&asked, "complete");
    assert_eq!(sent.len(), 2);
    assert_eq!(
        (sent[1]["id"].as_str(), sent[1]["token"].as_str()),
        (Some("4"), Some("lease-1"))
    );
    assert_eq!(sent[1]["remaining"], "the page");
    assert!(rig.lp(0).outbox.is_empty() && on_disk(&mut rig).is_empty());
    let _ = listed;
}

#[tokio::test]
async fn the_stores_answer_to_a_repeat_removes_the_entry() {
    let answer = (200, json!({ "value": { "built": true, "status": "merged" } }));
    let (mut rig, asked, _listed, _) = rig_owing(vec![answer]).await;
    rig.machine.pass().await;
    assert_eq!(ops_named(&asked, "complete").len(), 1);
    assert!(rig.lp(0).outbox.is_empty() && on_disk(&mut rig).is_empty());
    assert_eq!(errors(&mut rig, "store_error"), 0);
}

#[tokio::test]
async fn a_stale_lease_or_a_conflict_drops_the_entry_with_a_store_error() {
    for code in ["stale_lease", "conflict"] {
        let (mut rig, asked, _listed, _) = rig_owing(vec![refusal(409, code)]).await;
        rig.machine.pass().await;
        rig.machine.pass().await;
        assert_eq!(ops_named(&asked, "complete").len(), 1, "{code}: not sent again");
        assert!(rig.lp(0).outbox.is_empty() && on_disk(&mut rig).is_empty(), "{code}");
        assert_eq!(errors(&mut rig, "store_error"), 1, "{code}");
    }
}

#[tokio::test]
async fn an_entry_the_store_keeps_refusing_is_kept_and_retried_with_one_error_event_a_day() {
    let (mut rig, _asked, _listed, _) = rig_owing(vec![refusal(500, "boom"); 8]).await;
    rig.lp(0).clock = Some(1000 + DAY - 1);
    rig.machine.pass().await;
    assert!(rig.lp(0).outbox.contains_key("4"));
    assert_eq!(errors(&mut rig, "error"), 0);
    for at in [DAY, DAY + 60, 2 * DAY - 1] {
        rig.lp(0).clock = Some(1000 + at);
        rig.machine.pass().await;
        assert!(rig.lp(0).outbox.contains_key("4") && on_disk(&mut rig).len() == 1);
        assert_eq!(errors(&mut rig, "error"), 1, "at {at}");
    }
    rig.lp(0).clock = Some(1000 + 2 * DAY);
    rig.machine.pass().await;
    assert!(rig.lp(0).outbox.contains_key("4"));
    assert_eq!(errors(&mut rig, "error"), 2);
}

fn finish_row() -> Value {
    json!({ "id": "in-1", "kind": "finish_requested", "status": "requested", "issuedBy": "ana",
        "args": { "itemId": "4" } })
}

/// A finish that the store can't take: the row is refused and the item stays, the call is listed, and the next pass
/// completes it and lets the item go.
#[tokio::test]
async fn a_finish_the_store_didnt_take_is_listed_and_a_later_pass_finishes_it() {
    let (base, asked, listed) = store_failing("").await;
    *listed.complete_answers.lock().unwrap() = vec![refusal(500, "boom")];
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.machine.finish_host = Some(Arc::new(Head));
    rig.queue(0, &["4"]);
    let lp = rig.lp(0);
    lp.runs.get_mut("4").unwrap().state = ItemState::NeedsHuman;
    lp.leases.insert("4".into(), "lease-1".into());
    lp.adopted = true;
    lp.finishing.poll_every = Duration::ZERO;
    push(&listed, finish_row());
    let sent = until(&mut rig, &asked, 1).await;
    assert_eq!(sent[0]["status"], "refused");
    for _ in 0..3 {
        rig.machine.pass().await;
    }
    let completes = ops_named(&asked, "complete");
    assert_eq!(completes.len(), 2, "sent again by a later pass");
    assert_eq!(completes[1]["mergeCommit"], "abc123");
    let lp = rig.lp(0);
    assert!(lp.outbox.is_empty() && lp.runs.is_empty() && lp.leases.is_empty() && lp.records.is_empty());
}
