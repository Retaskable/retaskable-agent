//! The CI watcher's schedule on a project's loop (`driver/ci_pass.rs`): only the rollout's runner, only with the
//! backlog linked, one pass at a time and at most every 2 minutes. A fake host stands in for `gh` and the judge.
use std::sync::mpsc;
use std::time::Instant;

use super::*;
use crate::backlog::ci_result::{CiChecks, CiState, CiVerdict};
use crate::ci_pass::CiHost;
use crate::ci_pulls::OpenPull;
use crate::config::ProjectConfig;
use crate::orchestrator::driver::CiWatchHost;
use crate::rollout::Runners;

#[derive(Default)]
pub(super) struct Host {
    pulls: Mutex<Vec<OpenPull>>,
    listed: AtomicU32,
    checked: Mutex<Vec<String>>,
    /// While set, listing waits for a send: a pass that is still running.
    hold: Mutex<Option<mpsc::Receiver<()>>>,
}

struct Gh(Arc<Host>);

impl CiHost for Gh {
    fn open_pulls(&self) -> Result<Vec<OpenPull>> {
        self.0.listed.fetch_add(1, Ordering::SeqCst);
        if let Some(rx) = self.0.hold.lock().unwrap().take() {
            let _ = rx.recv();
        }
        Ok(self.0.pulls.lock().unwrap().clone())
    }
    fn checks(&self, sha: &str) -> Result<CiChecks> {
        self.0.checked.lock().unwrap().push(sha.to_string());
        Ok(CiChecks {
            state: CiState::Pass,
            what: String::new(),
            logs: vec![],
            ended_at: None,
        })
    }
    fn rerun(&self, _: u64, _: bool) -> bool {
        false
    }
    fn login(&self) -> Option<String> {
        None
    }
    fn base_head(&self) -> Option<String> {
        None
    }
    fn merge_main(&self, _: u64, _: &str) -> bool {
        false
    }
    fn fix_tree(&self, _: u64, _: &str) -> anyhow::Result<crate::ci_fix::FixTree> {
        anyhow::bail!("no worktree")
    }
}

/// The fake [`CiWatchHost`]: `Host` behind an `Arc`, so the fake code host and the test see the same counts.
pub(super) struct Shared(pub(super) Arc<Host>);

impl CiWatchHost for Shared {
    fn code_host(&self, _: &ProjectConfig) -> Arc<dyn CiHost> {
        Arc::new(Gh(self.0.clone()))
    }
    fn judge(
        &self,
        _: &ProjectConfig,
        _: u64,
        _: String,
        _: String,
        _: String,
    ) -> BoxFuture<'static, Option<(CiVerdict, String)>> {
        async { None }.boxed()
    }
    fn fix(
        &self,
        _: &ProjectConfig,
        _: u64,
        _: String,
        _: String,
        _: String,
        _: std::path::PathBuf,
    ) -> BoxFuture<'static, bool> {
        async { false }.boxed()
    }
    fn bell(&self, _: &str, _: &str) {}
}

fn pull(number: u64, branch: &str) -> OpenPull {
    OpenPull {
        number,
        title: format!("Title {number}"),
        url: String::new(),
        branch: branch.into(),
        head: format!("sha{number}"),
        author: None,
        mergeable: None,
        cross_repository: false,
    }
}

type Seen = Arc<Mutex<Vec<(String, Value)>>>;

/// A fake store that records each op with its arguments and answers `ci_result` with a green head.
async fn recording_store() -> (String, Seen) {
    let seen: Seen = Default::default();
    let log = seen.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let log = log.clone();
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                log.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = if op == "ci_result" {
                    json!({ "ok": true, "condition": "green" })
                } else {
                    json!([])
                };
                Json(json!({ "value": value }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

fn facts(seen: &Seen) -> Vec<Value> {
    (seen.lock().unwrap().iter())
        .filter(|(op, _)| op == "ci_result")
        .map(|(_, a)| a.clone())
        .collect()
}

async fn rig_watching(pulls: Vec<OpenPull>) -> (Rig, Arc<Host>, Arc<dyn CiWatchHost>, Seen) {
    let (base, seen) = recording_store().await;
    let mut rig = rig(1, &[&base]);
    rig.go_live();
    let host = Arc::new(Host {
        pulls: Mutex::new(pulls),
        ..Default::default()
    });
    let watch: Arc<dyn CiWatchHost> = Arc::new(Shared(host.clone()));
    (rig, host, watch, seen)
}

fn token() -> Option<String> {
    Some("tok".into())
}

#[tokio::test]
async fn a_computer_that_is_not_the_runner_lists_nothing() {
    let (mut rig, host, watch, seen) = rig_watching(vec![pull(1, "feat/a")]).await;
    let lp = rig.lp(0);
    lp.ci_pass(&watch, false, token(), false, Instant::now());
    lp.ci_watch.finish().await;
    assert_eq!(host.listed.load(Ordering::SeqCst), 0);
    assert!(facts(&seen).is_empty());
}

#[tokio::test]
async fn the_runner_sends_each_pull_requests_facts_with_may_push_false() {
    let (mut rig, host, watch, seen) = rig_watching(vec![pull(1, "feat/a"), pull(2, "feat/b")]).await;
    let lp = rig.lp(0);
    lp.ci_pass(&watch, true, token(), false, Instant::now());
    lp.ci_watch.finish().await;
    assert_eq!(host.listed.load(Ordering::SeqCst), 1);
    let sent = facts(&seen);
    assert_eq!(sent.len(), 2);
    assert!(
        sent.iter().all(|f| f["mayPush"] == false && f["button"] == false),
        "{sent:?}"
    );
    assert_eq!(sent[0]["sha"], "sha1");
}

#[tokio::test]
async fn nothing_runs_without_the_backlog_link_or_the_repos_id_or_while_stopping() {
    let (mut rig, host, watch, _) = rig_watching(vec![pull(1, "feat/a")]).await;
    let now = Instant::now();
    let lp = rig.lp(0);
    lp.ci_pass(&watch, true, None, false, now);
    lp.ci_pass(&watch, true, token(), true, now);
    lp.repo = None;
    lp.ci_pass(&watch, true, token(), false, now);
    lp.ci_watch.finish().await;
    assert_eq!(host.listed.load(Ordering::SeqCst), 0);
    // None of those spent the 2 minutes: the runner looks as soon as it can.
    lp.repo = Some(7);
    lp.ci_pass(&watch, true, token(), false, now);
    lp.ci_watch.finish().await;
    assert_eq!(host.listed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_pass_looks_at_most_every_two_minutes_and_never_two_at_once() {
    let (mut rig, host, watch, _) = rig_watching(vec![]).await;
    let t0 = Instant::now();
    let (release, hold) = mpsc::channel();
    *host.hold.lock().unwrap() = Some(hold);
    let lp = rig.lp(0);
    lp.ci_pass(&watch, true, token(), false, t0);
    // Still running three minutes on: no second one beside it.
    lp.ci_pass(&watch, true, token(), false, t0 + Duration::from_secs(180));
    release.send(()).unwrap();
    lp.ci_watch.finish().await;
    assert_eq!(host.listed.load(Ordering::SeqCst), 1);
    // The next one waits for two minutes after the last began.
    lp.ci_pass(&watch, true, token(), false, t0 + Duration::from_secs(119));
    lp.ci_watch.finish().await;
    assert_eq!(host.listed.load(Ordering::SeqCst), 1);
    lp.ci_pass(&watch, true, token(), false, t0 + Duration::from_secs(120));
    lp.ci_watch.finish().await;
    assert_eq!(host.listed.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn a_retaskable_branch_a_board_item_owns_is_skipped_from_the_loops_items() {
    let (mut rig, host, watch, seen) =
        rig_watching(vec![pull(1, "retaskable/1-mine"), pull(2, "retaskable/2-nobody")]).await;
    let item = json!({ "id": "mine", "title": "Mine", "status": "claimed", "rank": 1, "branch": "retaskable/1-mine" });
    let lp = rig.lp(0);
    lp.items.insert("mine".into(), serde_json::from_value(item).unwrap());
    lp.ci_pass(&watch, true, token(), false, Instant::now());
    lp.ci_watch.finish().await;
    assert_eq!(*host.checked.lock().unwrap(), ["sha2"]);
    assert_eq!(facts(&seen).len(), 1);
}

#[tokio::test]
async fn the_machine_asks_the_runners_flag_of_each_live_project() {
    for runner in [false, true] {
        let (mut rig, host, watch, _) = rig_watching(vec![]).await;
        let runners = Runners::default();
        runners.flag("p0").store(runner, Ordering::SeqCst);
        rig.machine.ci_watch = Some(watch);
        rig.machine.runners = runners;
        rig.machine.poll().await;
        rig.lp(0).ci_watch.finish().await;
        assert_eq!(host.listed.load(Ordering::SeqCst), u32::from(runner), "runner {runner}");
    }
}
