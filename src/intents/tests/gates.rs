//! The gates on rows that start work (`Handler::starts_work`, `runs_session`, `Gates`): a paused project and a computer
//! signed out of Claude leave such rows open, unclaimed, for another computer; at most two run at once on a computer,
//! across every puller. A kind that starts no work is taken whatever the gates say.
use tokio::sync::Semaphore;

use super::*;

/// A kind that waits to be let go before it ends, so a test can hold rows "running".
struct Held {
    kind: &'static str,
    starts_work: bool,
    runs_session: bool,
    gate: Arc<Semaphore>,
}

impl Held {
    fn new(kind: &'static str, starts_work: bool, runs_session: bool) -> (Arc<Held>, Arc<Semaphore>) {
        let gate = Arc::new(Semaphore::new(0));
        let h = Held {
            kind,
            starts_work,
            runs_session,
            gate: gate.clone(),
        };
        (Arc::new(h), gate)
    }
}

impl Handler<u32> for Held {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn starts_work(&self) -> bool {
        self.starts_work
    }
    fn runs_session(&self) -> bool {
        self.runs_session
    }
    fn run(&self, _row: IntentRow, _cx: &u32) -> BoxFuture<'static, Outcome> {
        let gate = self.gate.clone();
        async move {
            gate.acquire().await.unwrap().forget();
            Outcome::Done(json!({}))
        }
        .boxed()
    }
}

fn claims(fake: &Fake) -> Vec<String> {
    fake.log().into_iter().filter(|l| l.starts_with("claim")).collect()
}

fn store_of(fake: &Arc<Fake>) -> Arc<dyn IntentStore> {
    fake.clone()
}

fn held_puller(h: Arc<Held>) -> Puller<u32> {
    Puller::default()
        .register(h)
        .timing(Duration::from_secs(60), Duration::from_millis(1))
}

#[test]
fn a_handler_starts_no_work_and_runs_no_session_unless_it_says_so() {
    let (h, _) = Fixed::new("ask", Outcome::Done(json!({})));
    assert!(!Handler::<u32>::starts_work(&*h));
    assert!(!Handler::<u32>::runs_session(&*h));
}

#[tokio::test(start_paused = true)]
async fn a_paused_project_claims_no_row_that_starts_work_but_still_claims_the_rest() {
    let fake = Fake::with(vec![row("in-1", "fix_requested", "requested")]);
    let (h, gate) = Held::new("fix_requested", true, false);
    let mut p = held_puller(h);
    let paused = Gates {
        paused: true,
        ..Default::default()
    };
    assert_eq!(p.pass_gated(&store_of(&fake), &paused, || 0).await, 0);
    assert!(claims(&fake).is_empty());
    // Open again: the row was only left alone.
    gate.add_permits(1);
    assert_eq!(p.pass_gated(&store_of(&fake), &Gates::default(), || 0).await, 1);
    settle(&mut p).await;
    assert_eq!(claims(&fake), ["claim in-1"]);

    // A kind that starts no work is taken while paused.
    let fake = Fake::with(vec![row("in-2", "merge_requested", "requested")]);
    let (h, gate) = Held::new("merge_requested", false, false);
    gate.add_permits(1);
    let mut p = held_puller(h);
    assert_eq!(p.pass_gated(&store_of(&fake), &paused, || 0).await, 1);
    settle(&mut p).await;
}

#[tokio::test(start_paused = true)]
async fn a_computer_signed_out_of_claude_claims_no_row_that_runs_a_session() {
    let signed_out = Gates {
        signed_out: true,
        ..Default::default()
    };
    let fake = Fake::with(vec![row("in-1", "fix_requested", "requested")]);
    let (h, gate) = Held::new("fix_requested", true, true);
    let mut p = held_puller(h);
    assert_eq!(p.pass_gated(&store_of(&fake), &signed_out, || 0).await, 0);
    assert!(claims(&fake).is_empty());
    gate.add_permits(1);
    assert_eq!(p.pass_gated(&store_of(&fake), &Gates::default(), || 0).await, 1);
    settle(&mut p).await;

    // Work that runs no session doesn't need Claude.
    let fake = Fake::with(vec![row("in-2", "build", "requested")]);
    let (h, gate) = Held::new("build", true, false);
    gate.add_permits(1);
    let mut p = held_puller(h);
    assert_eq!(p.pass_gated(&store_of(&fake), &signed_out, || 0).await, 1);
    settle(&mut p).await;
}

#[tokio::test(start_paused = true)]
async fn at_most_two_rows_that_start_work_run_at_once_and_a_third_waits_unclaimed() {
    let rows = ["a", "b", "c"].map(|id| row(id, "fix_requested", "requested")).to_vec();
    let fake = Fake::with(rows);
    let (h, gate) = Held::new("fix_requested", true, true);
    let mut p = held_puller(h);
    let gates = Gates::default();
    assert_eq!(p.pass_gated(&store_of(&fake), &gates, || 0).await, 2);
    assert_eq!(claims(&fake), ["claim a", "claim b"]);
    assert_eq!(gates.work.running(), 2);
    // Still two running: a third pass claims nothing more.
    assert_eq!(p.pass_gated(&store_of(&fake), &gates, || 0).await, 0);
    assert_eq!(claims(&fake), ["claim a", "claim b"]);
    // One ends: the room is one, and the third takes it.
    gate.add_permits(1);
    while gates.work.running() == 2 {
        tokio::task::yield_now().await;
    }
    assert_eq!(p.pass_gated(&store_of(&fake), &gates, || 0).await, 1);
    assert_eq!(claims(&fake), ["claim a", "claim b", "claim c"]);
    gate.add_permits(2);
    settle(&mut p).await;
    assert_eq!(gates.work.running(), 0);
}

#[tokio::test(start_paused = true)]
async fn the_cap_is_the_computers_across_every_puller_and_rows_that_start_no_work_dont_count() {
    let gates = Gates::default();
    let fake_a = Fake::with(vec![row("a", "fix_requested", "requested")]);
    let fake_b = Fake::with(vec![row("b", "fix_requested", "requested")]);
    let fake_c = Fake::with(vec![row("c", "fix_requested", "requested")]);
    let (h, gate) = Held::new("fix_requested", true, true);
    let mut pullers: Vec<Puller<u32>> = (0..3).map(|_| held_puller(h.clone())).collect();
    for (p, fake) in pullers.iter_mut().zip([&fake_a, &fake_b, &fake_c]) {
        p.pass_gated(&store_of(fake), &gates, || 0).await;
    }
    assert_eq!(
        (claims(&fake_a).len(), claims(&fake_b).len(), claims(&fake_c).len()),
        (1, 1, 0)
    );
    // A quick row is taken beside them.
    let quick = Fake::with(vec![row("q", "merge_requested", "requested")]);
    let (q, qgate) = Held::new("merge_requested", false, false);
    qgate.add_permits(1);
    let mut qp = held_puller(q);
    assert_eq!(qp.pass_gated(&store_of(&quick), &gates, || 0).await, 1);
    settle(&mut qp).await;
    gate.add_permits(2);
    for p in &mut pullers {
        settle(p).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_row_held_before_a_restart_counts_toward_the_cap_too() {
    // A not-twice kind that starts work, found claimed: it is failed unrun, and counted while that goes out.
    struct Twice;
    impl Handler<u32> for Twice {
        fn kind(&self) -> &'static str {
            "fix_requested"
        }
        fn starts_work(&self) -> bool {
            true
        }
        fn not_twice(&self) -> bool {
            true
        }
        fn run(&self, _: IntentRow, _: &u32) -> BoxFuture<'static, Outcome> {
            unreachable!("a not-twice row held before a restart is not run")
        }
    }
    let fake = Fake::with(vec![row("a", "fix_requested", "claimed")]);
    let mut p = Puller::<u32>::default()
        .register(Arc::new(Twice))
        .timing(Duration::from_secs(60), Duration::from_millis(1));
    let gates = Gates::default();
    assert_eq!(p.pass_gated(&store_of(&fake), &gates, || 0).await, 1);
    settle(&mut p).await;
    assert_eq!(gates.work.running(), 0);
    assert!(fake.log().last().unwrap().starts_with("finish a Failed"));
}

#[test]
fn a_computer_says_why_it_takes_nothing_paused_first_then_signed_out_then_out_of_time() {
    let out = Some(OutOfTime { until_ms: Some(9) });
    let at = |paused, signed_out, out_of_time| {
        Gates {
            paused,
            signed_out,
            out_of_time,
            ..Default::default()
        }
        .block()
    };
    assert_eq!(at(true, true, out).why, Some("paused"));
    assert_eq!(at(false, true, out).why, Some("signed_out"));
    assert_eq!(
        at(false, false, out),
        Block {
            why: Some("out_of_time"),
            until_ms: Some(9)
        }
    );
    assert_eq!(at(false, false, None), Block::default());
}

#[tokio::test(start_paused = true)]
async fn a_pass_that_reports_tells_the_store_why_after_listing_and_one_that_does_not_stays_silent() {
    let fake = Fake::with(vec![]);
    let (h, _gate) = Held::new("ask", false, false);
    let mut p = held_puller(h);
    let paused = Gates {
        paused: true,
        report: true,
        ..Default::default()
    };
    p.pass_gated(&store_of(&fake), &paused, || 0).await;
    assert_eq!(fake.log(), ["list ask", "seen paused null"]);
    let limit = Gates {
        out_of_time: Some(OutOfTime { until_ms: Some(5000) }),
        report: true,
        ..Default::default()
    };
    p.pass_gated(&store_of(&fake), &limit, || 0).await;
    assert_eq!(fake.log()[3], "seen out_of_time 5000");
    let free = Gates {
        report: true,
        ..Default::default()
    };
    p.pass_gated(&store_of(&fake), &free, || 0).await;
    assert_eq!(fake.log()[5], "seen - null");
    // A puller that doesn't report (every other kind's) never overwrites what the reporting one said.
    let quiet = Fake::with(vec![]);
    p.pass(&store_of(&quiet), || 0).await;
    assert_eq!(quiet.log(), ["list ask"]);
}

/// Refuses its row, as a session that can't start does.
struct Refusing(&'static str, bool);

impl Handler<u32> for Refusing {
    fn kind(&self) -> &'static str {
        self.0
    }
    fn runs_session(&self) -> bool {
        self.1
    }
    fn run(&self, _row: IntentRow, _cx: &u32) -> BoxFuture<'static, Outcome> {
        async { Outcome::Refused("Out of Claude time".into()) }.boxed()
    }
}

fn refusing_puller(h: Refusing) -> Puller<u32> {
    Puller::default()
        .register(Arc::new(h))
        .timing(Duration::from_secs(60), Duration::from_millis(1))
}

#[tokio::test(start_paused = true)]
async fn a_session_row_refused_while_out_of_claude_time_is_released_for_another_computer() {
    let fake = Fake::with(vec![row("in-1", "design_draw_requested", "requested")]);
    let mut p = refusing_puller(Refusing("design_draw_requested", true));
    let out = Gates {
        out_of_time: Some(OutOfTime::default()),
        ..Default::default()
    };
    p.pass_gated(&store_of(&fake), &out, || 0).await;
    settle(&mut p).await;
    let log = fake.log();
    assert!(
        log.contains(&"release in-1 \"Out of Claude time\"".to_string()),
        "{log:?}"
    );
    assert!(!log.iter().any(|l| l.starts_with("finish")), "{log:?}");
}

#[tokio::test(start_paused = true)]
async fn a_refusal_is_finished_as_before_when_the_computer_has_time_or_the_row_runs_no_session() {
    // Free: refused as ever.
    let fake = Fake::with(vec![row("in-1", "design_draw_requested", "requested")]);
    let mut p = refusing_puller(Refusing("design_draw_requested", true));
    p.pass_gated(&store_of(&fake), &Gates::default(), || 0).await;
    settle(&mut p).await;
    assert!(fake.log().iter().any(|l| l.starts_with("finish in-1 Refused")));
    assert!(!fake.log().iter().any(|l| l.starts_with("release")));
    // Out of time, but the kind runs no session (a skip): a refusal is the row's end.
    let fake = Fake::with(vec![row("in-2", "design_skip_requested", "requested")]);
    let mut p = refusing_puller(Refusing("design_skip_requested", false));
    let out = Gates {
        out_of_time: Some(OutOfTime::default()),
        ..Default::default()
    };
    p.pass_gated(&store_of(&fake), &out, || 0).await;
    settle(&mut p).await;
    assert!(fake.log().iter().any(|l| l.starts_with("finish in-2 Refused")));
}

#[tokio::test(start_paused = true)]
async fn a_release_the_store_does_not_take_ends_the_row_refused() {
    let fake = Fake::with(vec![row("in-1", "design_draw_requested", "requested")]);
    fake.releases.lock().unwrap().push_back(Ok(false));
    let mut p = refusing_puller(Refusing("design_draw_requested", true));
    let out = Gates {
        out_of_time: Some(OutOfTime::default()),
        ..Default::default()
    };
    p.pass_gated(&store_of(&fake), &out, || 0).await;
    settle(&mut p).await;
    assert!(fake.log().iter().any(|l| l.starts_with("finish in-1 Refused")));
}
