//! The trial plan's step (`driver/trial.rs`): a fake store whose backlog is `listed`, and a fake host whose end is scripted.
use super::*;
use crate::orchestrator::trial_host::{TrialEnd, TrialHost, TrialJob};
use crate::setup_check::{SetupCheck, SetupChecks};
use crate::telemetry::Event;
use crate::trial_plan::{TRIAL_BRANCH, TrialRecord, TrialState};

#[derive(Default)]
struct Host {
    end: Mutex<Option<TrialEnd>>,
    jobs: Mutex<Vec<(u32, String)>>,
    /// The saved state when `run` was called.
    seen: Mutex<Vec<TrialState>>,
    hold: AtomicBool,
    state_path: Mutex<Option<std::path::PathBuf>>,
}

impl TrialHost for Host {
    fn run(&self, job: TrialJob) -> BoxFuture<'static, Result<TrialEnd>> {
        self.jobs.lock().unwrap().push((job.lane, job.branch.clone()));
        if let Some(p) = self.state_path.lock().unwrap().as_ref() {
            let s = Store::load(p).unwrap();
            self.seen.lock().unwrap().push(s.trial().state);
        }
        let end = self
            .end
            .lock()
            .unwrap()
            .take()
            .unwrap_or(TrialEnd::Dropped { reason: "none".into() });
        let hold = self.hold.load(Ordering::SeqCst);
        async move {
            if hold {
                std::future::pending::<()>().await;
            }
            Ok(end)
        }
        .boxed()
    }
}

fn check(ok: bool) -> SetupCheck {
    SetupCheck {
        ok: Some(ok),
        detail: String::new(),
        fix: None,
    }
}

fn checks(ok: bool) -> SetupChecks {
    SetupChecks {
        git: check(ok),
        gh_installed: check(ok),
        gh: check(ok),
        claude: check(ok),
        claude_signed_in: check(ok),
        cursor: check(false),
        pi: None,
        codex: None,
        docker: None,
    }
}

struct T {
    rig: Rig,
    host: Arc<Host>,
    checks: super::super::machine::SetupChecksHandle,
}

async fn rig_with(listed: Value, end: TrialEnd) -> T {
    let (base, _asked) = claim_stub(listed, Some(Value::Null)).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.lp(0).project.github_repo = "o/r".into();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["trial-plan-draft"]));
    let host = Arc::new(Host {
        end: Mutex::new(Some(end)),
        ..Default::default()
    });
    *host.state_path.lock().unwrap() = Some(rig._dir.path().join("0"));
    let checks = Arc::new(Mutex::new(Some(checks(true))));
    rig.machine.trial = Some((host.clone(), checks.clone()));
    T { rig, host, checks }
}

async fn empty(end: TrialEnd) -> T {
    rig_with(json!([]), end).await
}

fn opened() -> TrialEnd {
    TrialEnd::Opened {
        pr_url: "https://github.com/o/r/pull/3".into(),
    }
}

fn saved(t: &mut T) -> TrialRecord {
    t.rig.lp(0).store.trial()
}

async fn passes(t: &mut T, n: usize) {
    for _ in 0..n {
        t.rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn held_by_trial(t: &mut T) -> bool {
    let holder = Holder::Trial { project: "p0".into() };
    (1..=2).any(|k| t.rig.machine.pool.holder(k) == Some(&holder))
}

#[tokio::test]
async fn an_empty_backlog_starts_one_trial_in_a_free_lane_and_saves_running_first() {
    let mut t = empty(opened()).await;
    t.host.hold.store(true, Ordering::SeqCst);
    passes(&mut t, 3).await;
    assert_eq!(*t.host.jobs.lock().unwrap(), vec![(1, TRIAL_BRANCH.to_string())]);
    assert_eq!(*t.host.seen.lock().unwrap(), vec![TrialState::Running]);
    assert_eq!(saved(&mut t).state, TrialState::Running);
    assert!(held_by_trial(&mut t));
}

#[tokio::test]
async fn opened_saves_open_with_the_url_logs_it_and_frees_the_lane() {
    let mut t = empty(opened()).await;
    passes(&mut t, 3).await;
    let r = saved(&mut t);
    assert_eq!(
        (r.state, r.pr_url.as_deref()),
        (TrialState::Open, Some("https://github.com/o/r/pull/3"))
    );
    assert!(kinds(&mut t.rig).contains(&"trial_opened".to_string()));
    assert!(!held_by_trial(&mut t));
    passes(&mut t, 2).await;
    assert_eq!(t.host.jobs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn dropped_saves_failed_logs_why_and_frees_the_lane() {
    let mut t = empty(TrialEnd::Dropped {
        reason: "no push".into(),
    })
    .await;
    passes(&mut t, 3).await;
    assert_eq!(saved(&mut t).state, TrialState::Failed);
    assert!(!held_by_trial(&mut t));
    let log = t.rig.lp(0).events().recent().to_vec();
    let e = log.iter().find(|e| e.kind() == "trial_dropped").expect("logged");
    assert!(matches!(e.event(), Some(Event::TrialDropped(d)) if d.reason.as_deref() == Some("no push")));
    passes(&mut t, 2).await;
    assert_eq!(t.host.jobs.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_backlog_with_items_saves_skipped_and_never_starts_one_later() {
    let mut t = rig_with(json!([item("a", "on_main")]), opened()).await;
    passes(&mut t, 2).await;
    assert!(saved(&mut t).skipped);
    t.rig.lp(0).backlog_items = 0;
    passes(&mut t, 2).await;
    assert!(t.host.jobs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn it_doesnt_start_when_paused_flagged_off_failing_unchecked_or_full() {
    let mut t = empty(opened()).await;
    t.rig.pause("p0", true);
    passes(&mut t, 2).await;
    t.rig.pause("p0", false);
    t.rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&[]));
    passes(&mut t, 2).await;
    t.rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["trial-plan-draft"]));
    *t.checks.lock().unwrap() = Some(checks(false));
    passes(&mut t, 2).await;
    *t.checks.lock().unwrap() = None;
    passes(&mut t, 2).await;
    *t.checks.lock().unwrap() = Some(checks(true));
    for k in 1..=2 {
        t.rig.machine.pool.take(
            k,
            Holder::Item {
                project: "p1".into(),
                item: format!("x{k}"),
            },
        );
    }
    passes(&mut t, 2).await;
    assert!(t.host.jobs.lock().unwrap().is_empty());
    assert_eq!(saved(&mut t), TrialRecord::default());
}

#[tokio::test]
async fn a_restart_that_finds_running_starts_no_second_trial() {
    let mut t = empty(opened()).await;
    t.rig.lp(0).store.set_trial(TrialRecord::started()).unwrap();
    passes(&mut t, 3).await;
    assert!(t.host.jobs.lock().unwrap().is_empty());
    assert_eq!(saved(&mut t).state, TrialState::Failed);
    assert!(kinds(&mut t.rig).contains(&"trial_dropped".to_string()));
}
