use std::collections::VecDeque;

use serde_json::json;

use super::*;

#[derive(Default)]
struct Fake {
    rows: Mutex<Vec<IntentRow>>,
    /// Claim answers in order; `won` once the queue is empty.
    claims: Mutex<VecDeque<Result<Claim, BacklogError>>>,
    /// Finish answers in order; `Ok(true)` once the queue is empty.
    finishes: Mutex<VecDeque<Result<bool, BacklogError>>>,
    seen: Mutex<Vec<String>>,
}

fn row(id: &str, kind: &str, status: &str) -> IntentRow {
    IntentRow {
        id: id.into(),
        kind: kind.into(),
        args: json!({ "question": "where?" }),
        issued_by: Some("ana".into()),
        status: status.into(),
    }
}

fn bad() -> BacklogError {
    BacklogError::Unreachable("down".into())
}

impl Fake {
    fn with(rows: Vec<IntentRow>) -> Arc<Fake> {
        Arc::new(Fake {
            rows: Mutex::new(rows),
            ..Default::default()
        })
    }
    fn log(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
    fn note(&self, line: String) {
        self.seen.lock().unwrap().push(line);
    }
}

impl IntentStore for Fake {
    fn list(&self, kinds: &[&str]) -> BoxFuture<'static, Result<Vec<IntentRow>, BacklogError>> {
        self.note(format!("list {}", kinds.join(",")));
        let rows = self.rows.lock().unwrap().clone();
        async move { Ok(rows) }.boxed()
    }
    fn claim(&self, id: &str) -> BoxFuture<'static, Result<Claim, BacklogError>> {
        self.note(format!("claim {id}"));
        let answer = self.claims.lock().unwrap().pop_front().unwrap_or(Ok(Claim {
            won: true,
            blocked: false,
        }));
        async move { answer }.boxed()
    }
    fn beat(&self, id: &str) -> BoxFuture<'static, Result<bool, BacklogError>> {
        self.note(format!("beat {id}"));
        async { Ok(true) }.boxed()
    }
    fn finish(
        &self,
        id: &str,
        ending: Ending,
        result: Option<Value>,
        why: Option<String>,
    ) -> BoxFuture<'static, Result<bool, BacklogError>> {
        self.note(format!("finish {id} {ending:?} {} {}", json!(result), json!(why)));
        let answer = self.finishes.lock().unwrap().pop_front().unwrap_or(Ok(true));
        async move { answer }.boxed()
    }
}

/// Answers every row of its kind with a fixed outcome, counting its runs.
struct Fixed {
    kind: &'static str,
    outcome: Outcome,
    only: Option<&'static str>,
    runs: Arc<Mutex<Vec<String>>>,
}

impl Fixed {
    fn new(kind: &'static str, outcome: Outcome) -> (Arc<Fixed>, Arc<Mutex<Vec<String>>>) {
        let runs = Arc::new(Mutex::new(Vec::new()));
        let h = Fixed {
            kind,
            outcome,
            only: None,
            runs: runs.clone(),
        };
        (Arc::new(h), runs)
    }
}

impl Handler<u32> for Fixed {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn takes(&self, row: &IntentRow) -> bool {
        self.only.is_none_or(|id| id == row.id)
    }
    fn run(&self, row: IntentRow, cx: &u32) -> BoxFuture<'static, Outcome> {
        self.runs.lock().unwrap().push(format!("{} cx={cx}", row.id));
        let outcome = self.outcome.clone();
        async move { outcome }.boxed()
    }
}

async fn settle(puller: &mut Puller<u32>) {
    while puller.active() > 0 {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn puller(h: Arc<Fixed>) -> Puller<u32> {
    Puller::default()
        .register(h)
        .timing(Duration::from_secs(60), Duration::from_millis(1))
}

#[tokio::test(start_paused = true)]
async fn a_won_row_is_beaten_run_and_finished_done() {
    let fake = Fake::with(vec![row("in-1", "ask", "requested")]);
    let (h, runs) = Fixed::new("ask", Outcome::Done(json!({ "answer": "In a.rs." })));
    let mut p = puller(h);
    let store: Arc<dyn IntentStore> = fake.clone();
    assert_eq!(p.pass(&store, || 7).await, 1);
    settle(&mut p).await;
    assert_eq!(*runs.lock().unwrap(), ["in-1 cx=7"]);
    assert_eq!(
        fake.log(),
        [
            "list ask",
            "claim in-1",
            "beat in-1",
            r#"finish in-1 Done {"answer":"In a.rs."} null"#
        ]
    );
}

#[tokio::test(start_paused = true)]
async fn refused_and_failed_carry_their_reason() {
    for (outcome, line) in [
        (
            Outcome::Refused("Not now.".into()),
            r#"finish in-1 Refused null "Not now.""#,
        ),
        (Outcome::Failed("Broke.".into()), r#"finish in-1 Failed null "Broke.""#),
    ] {
        let fake = Fake::with(vec![row("in-1", "ask", "requested")]);
        let (h, _) = Fixed::new("ask", outcome);
        let mut p = puller(h);
        let store: Arc<dyn IntentStore> = fake.clone();
        p.pass(&store, || 0).await;
        settle(&mut p).await;
        assert_eq!(fake.log().last().unwrap(), line);
    }
}

#[tokio::test(start_paused = true)]
async fn a_lost_blocked_or_failed_claim_runs_nothing() {
    let fake = Fake::with(vec![
        row("a", "ask", "requested"),
        row("b", "ask", "requested"),
        row("c", "ask", "requested"),
    ]);
    *fake.claims.lock().unwrap() = VecDeque::from([
        Ok(Claim {
            won: false,
            blocked: false,
        }),
        Ok(Claim {
            won: false,
            blocked: true,
        }),
        Err(bad()),
    ]);
    let (h, runs) = Fixed::new("ask", Outcome::Done(json!({})));
    let mut p = puller(h);
    let store: Arc<dyn IntentStore> = fake.clone();
    assert_eq!(p.pass(&store, || 0).await, 0);
    assert!(runs.lock().unwrap().is_empty());
    assert_eq!(fake.log(), ["list ask", "claim a", "claim b", "claim c"]);
    // Nothing was taken, so the next pass asks again and wins.
    assert_eq!(p.pass(&store, || 0).await, 3);
    settle(&mut p).await;
}

#[tokio::test(start_paused = true)]
async fn rows_that_are_finished_or_of_another_kind_or_not_taken_are_skipped() {
    let fake = Fake::with(vec![
        row("done", "ask", "done"),
        row("other", "take_over", "requested"),
        row("no", "ask", "requested"),
        row("yes", "ask", "requested"),
    ]);
    let runs = Arc::new(Mutex::new(Vec::new()));
    let h = Arc::new(Fixed {
        kind: "ask",
        outcome: Outcome::Done(json!({})),
        only: Some("yes"),
        runs: runs.clone(),
    });
    let mut p = puller(h);
    let store: Arc<dyn IntentStore> = fake.clone();
    assert_eq!(p.pass(&store, || 0).await, 1);
    settle(&mut p).await;
    assert_eq!(*runs.lock().unwrap(), ["yes cx=0"]);
    assert!(
        !fake
            .log()
            .iter()
            .any(|l| l == "claim done" || l == "claim other" || l == "claim no")
    );
}

#[tokio::test(start_paused = true)]
async fn a_row_is_never_run_twice_while_the_store_still_lists_it() {
    let fake = Fake::with(vec![row("in-1", "ask", "claimed")]);
    let (h, runs) = Fixed::new("ask", Outcome::Done(json!({})));
    let mut p = puller(h);
    let store: Arc<dyn IntentStore> = fake.clone();
    // A claimed row of this computer's own (before a restart) is taken.
    assert_eq!(p.pass(&store, || 0).await, 1);
    settle(&mut p).await;
    // The store hasn't caught up: the row is still listed. It is not run again.
    assert_eq!(p.pass(&store, || 0).await, 0);
    assert_eq!(runs.lock().unwrap().len(), 1);
    // Once the store forgets it, a new row with the same id is new.
    fake.rows.lock().unwrap().clear();
    p.pass(&store, || 0).await;
    fake.rows.lock().unwrap().push(row("in-1", "ask", "requested"));
    assert_eq!(p.pass(&store, || 0).await, 1);
    settle(&mut p).await;
}

#[tokio::test(start_paused = true)]
async fn the_context_is_built_once_and_only_for_a_won_row() {
    let fake = Fake::with(vec![row("a", "ask", "requested"), row("b", "ask", "requested")]);
    let (h, _) = Fixed::new("ask", Outcome::Done(json!({})));
    let mut p = puller(h);
    let store: Arc<dyn IntentStore> = fake.clone();
    let built = Mutex::new(0);
    p.pass(&store, || {
        *built.lock().unwrap() += 1;
        1
    })
    .await;
    assert_eq!(*built.lock().unwrap(), 1);
    settle(&mut p).await;
    let lost = Fake::with(vec![row("a", "ask", "requested")]);
    *lost.claims.lock().unwrap() = VecDeque::from([Ok(Claim::default())]);
    let lost: Arc<dyn IntentStore> = lost;
    let (h, _) = Fixed::new("ask", Outcome::Done(json!({})));
    let mut p = puller(h);
    p.pass(&lost, || panic!("built for a lost row")).await;
}

/// Never ends until told; beats are counted by the fake.
struct Slow(Arc<tokio::sync::Notify>);

impl Handler<u32> for Slow {
    fn kind(&self) -> &'static str {
        "ask"
    }
    fn run(&self, _: IntentRow, _: &u32) -> BoxFuture<'static, Outcome> {
        let go = self.0.clone();
        async move {
            go.notified().await;
            Outcome::Done(json!({ "answer": "late" }))
        }
        .boxed()
    }
}

#[tokio::test(start_paused = true)]
async fn a_running_row_beats_every_interval_until_it_ends() {
    let fake = Fake::with(vec![row("in-1", "ask", "requested")]);
    let go = Arc::new(tokio::sync::Notify::new());
    let mut p: Puller<u32> = Puller::default().register(Arc::new(Slow(go.clone())));
    let store: Arc<dyn IntentStore> = fake.clone();
    p.pass(&store, || 0).await;
    let beats = |f: &Fake| f.log().iter().filter(|l| l.starts_with("beat")).count();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(beats(&fake), 1, "the first beat is at once");
    tokio::time::sleep(BEAT_EVERY * 2).await;
    assert_eq!(beats(&fake), 3);
    assert_eq!(p.active(), 1);
    go.notify_one();
    settle(&mut p).await;
    assert!(fake.log().last().unwrap().starts_with("finish in-1 Done"));
    tokio::time::sleep(BEAT_EVERY * 3).await;
    assert_eq!(beats(&fake), 3, "no beats after the finish");
}

#[tokio::test(start_paused = true)]
async fn a_finish_the_store_refuses_is_tried_three_times() {
    let fake = Fake::with(vec![row("in-1", "ask", "requested")]);
    *fake.finishes.lock().unwrap() = VecDeque::from([Err(bad()), Err(bad()), Ok(true)]);
    let (h, _) = Fixed::new("ask", Outcome::Done(json!({})));
    let mut p = puller(h);
    let store: Arc<dyn IntentStore> = fake.clone();
    p.pass(&store, || 0).await;
    settle(&mut p).await;
    let finishes = fake.log().iter().filter(|l| l.starts_with("finish")).count();
    assert_eq!(finishes, 3);

    let fake = Fake::with(vec![row("in-2", "ask", "requested")]);
    *fake.finishes.lock().unwrap() = VecDeque::from([Err(bad()), Err(bad()), Err(bad()), Ok(true)]);
    let (h, _) = Fixed::new("ask", Outcome::Done(json!({})));
    let mut p = puller(h);
    let store: Arc<dyn IntentStore> = fake.clone();
    p.pass(&store, || 0).await;
    settle(&mut p).await;
    let finishes = fake.log().iter().filter(|l| l.starts_with("finish")).count();
    assert_eq!(finishes, 3, "gives up after three; the row is not run again");
    assert_eq!(p.pass(&store, || 0).await, 0);
}

#[tokio::test(start_paused = true)]
async fn an_abandoned_row_says_nothing_to_the_store() {
    let fake = Fake::with(vec![row("in-1", "ask", "requested")]);
    let (h, _) = Fixed::new("ask", Outcome::Abandoned);
    let mut p = puller(h);
    let store: Arc<dyn IntentStore> = fake.clone();
    p.pass(&store, || 0).await;
    settle(&mut p).await;
    assert!(!fake.log().iter().any(|l| l.starts_with("finish")));
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_store_starts_nothing_and_no_handlers_never_lists() {
    struct Down;
    impl IntentStore for Down {
        fn list(&self, _: &[&str]) -> BoxFuture<'static, Result<Vec<IntentRow>, BacklogError>> {
            async { Err(bad()) }.boxed()
        }
        fn claim(&self, _: &str) -> BoxFuture<'static, Result<Claim, BacklogError>> {
            unreachable!()
        }
        fn beat(&self, _: &str) -> BoxFuture<'static, Result<bool, BacklogError>> {
            unreachable!()
        }
        fn finish(
            &self,
            _: &str,
            _: Ending,
            _: Option<Value>,
            _: Option<String>,
        ) -> BoxFuture<'static, Result<bool, BacklogError>> {
            unreachable!()
        }
    }
    let down: Arc<dyn IntentStore> = Arc::new(Down);
    let (h, _) = Fixed::new("ask", Outcome::Done(json!({})));
    assert_eq!(puller(h).pass(&down, || 0).await, 0);

    let fake = Fake::with(vec![row("in-1", "ask", "requested")]);
    let store: Arc<dyn IntentStore> = fake.clone();
    assert_eq!(Puller::<u32>::default().pass(&store, || 0).await, 0);
    assert!(fake.log().is_empty());
}

#[tokio::test(start_paused = true)]
async fn adding_a_kind_is_one_registration() {
    let fake = Fake::with(vec![row("a", "ask", "requested"), row("b", "sync", "requested")]);
    let (ask, ask_runs) = Fixed::new("ask", Outcome::Done(json!({})));
    let (sync, sync_runs) = Fixed::new("sync", Outcome::Done(json!({})));
    let mut p: Puller<u32> = Puller::default()
        .register(ask)
        .register(sync)
        .timing(Duration::from_secs(60), Duration::from_millis(1));
    let store: Arc<dyn IntentStore> = fake.clone();
    assert_eq!(p.pass(&store, || 0).await, 2);
    settle(&mut p).await;
    assert_eq!(fake.log()[0], "list ask,sync");
    assert_eq!(ask_runs.lock().unwrap().len(), 1);
    assert_eq!(sync_runs.lock().unwrap().len(), 1);
}
