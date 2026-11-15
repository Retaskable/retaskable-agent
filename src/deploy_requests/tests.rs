//! The deploy request loop against fakes of the store and the code that runs a request (tokio's paused time).
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::backlog::deploy::{DeployArgs, DeployKind};
use crate::config::ProjectConfig;

const NOW: u64 = 1_000_000;

fn request(id: &str, kind: DeployKind) -> DeployRequest {
    DeployRequest {
        id: id.into(),
        kind,
        args: DeployArgs::default(),
        issued_by: "Ann".into(),
        at: NOW - 60_000,
        status: DeployStatus::Requested,
    }
}

type Finished = (String, Finish, Option<Map<String, Value>>);

#[derive(Default)]
struct State {
    rows: Vec<DeployRequest>,
    list_error: bool,
    cannot: Vec<DeployKind>,
    lose: bool,
    claims: Vec<String>,
    beats: usize,
    ran: Vec<String>,
    runs: VecDeque<Result<Outcome, RunError>>,
    run_for: Duration,
    finished: Vec<Finished>,
}

#[derive(Clone, Default)]
struct Fake(Arc<StdMutex<State>>);

impl Fake {
    fn with(rows: Vec<DeployRequest>) -> Self {
        let f = Self::default();
        f.0.lock().unwrap().rows = rows;
        f
    }
    fn state<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        f(&mut self.0.lock().unwrap())
    }
}

impl Deps for Fake {
    fn machine(&self) -> String {
        "a-pc".into()
    }
    fn now_ms(&self) -> u64 {
        NOW
    }
    async fn list(&self) -> Result<Vec<DeployRequest>, String> {
        self.state(|s| {
            if s.list_error {
                Err("the store is down".into())
            } else {
                Ok(s.rows.clone())
            }
        })
    }
    async fn can(&self, r: &DeployRequest) -> bool {
        self.state(|s| !s.cannot.contains(&r.kind))
    }
    async fn claim(&self, id: &str) -> Result<bool, String> {
        self.state(|s| {
            s.claims.push(id.into());
            Ok(!s.lose)
        })
    }
    async fn beat(&self, _: &str) -> Result<bool, String> {
        self.state(|s| s.beats += 1);
        Ok(true)
    }
    async fn finish(&self, id: &str, status: Finish, result: Option<Map<String, Value>>) -> Result<bool, String> {
        self.state(|s| s.finished.push((id.into(), status, result)));
        Ok(true)
    }
    async fn run(&self, r: &DeployRequest) -> Result<Outcome, RunError> {
        let wait = self.state(|s| {
            s.ran.push(r.id.clone());
            s.run_for
        });
        tokio::time::sleep(wait).await;
        self.state(|s| s.runs.pop_front())
            .unwrap_or_else(|| Ok(done(Map::new())))
    }
}

fn map(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// Lets the spawned runs finish.
async fn settle() {
    tokio::time::sleep(Duration::from_millis(10)).await;
}

#[tokio::test(start_paused = true)]
async fn a_request_this_computer_cannot_run_is_left_unclaimed() {
    let fake = Fake::with(vec![request("dr-1", DeployKind::TestflightBuild)]);
    fake.state(|s| s.cannot = vec![DeployKind::TestflightBuild]);
    let mut loop_ = DeployRequests::new(fake.clone());
    assert_eq!(loop_.cycle().await, WAIT);
    settle().await;
    fake.state(|s| {
        assert!(s.claims.is_empty());
        assert!(s.ran.is_empty());
        assert!(s.finished.is_empty());
    });
}

#[tokio::test(start_paused = true)]
async fn a_request_it_can_run_is_claimed_run_and_finished_with_the_derived_result() {
    let fake = Fake::with(vec![request("dr-1", DeployKind::Hosting)]);
    fake.state(|s| s.runs.push_back(Ok(done(map(json!({ "pick": "github-pages" }))))));
    let mut loop_ = DeployRequests::new(fake.clone());
    loop_.cycle().await;
    settle().await;
    fake.state(|s| {
        assert_eq!(s.claims, ["dr-1"]);
        assert_eq!(s.ran, ["dr-1"]);
        assert!(s.beats >= 1, "the first beat marks it running");
        assert_eq!(
            s.finished,
            [(
                "dr-1".to_string(),
                Finish::Done,
                Some(map(json!({ "pick": "github-pages" })))
            )]
        );
    });
    assert_eq!(loop_.running(), 0);
}

#[tokio::test(start_paused = true)]
async fn a_refusal_is_refused_with_its_reason_and_any_other_error_is_failed() {
    let fake = Fake::with(vec![
        request("dr-1", DeployKind::Promote),
        request("dr-2", DeployKind::GoLivePrepare),
    ]);
    fake.state(|s| {
        s.runs.push_back(Err(RunError::Refused("Name a commit.".into())));
        s.runs.push_back(Err(RunError::Failed("git exploded".into())));
    });
    DeployRequests::new(fake.clone()).cycle().await;
    settle().await;
    fake.state(|s| {
        let mut done = s.finished.clone();
        done.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            done,
            [
                (
                    "dr-1".to_string(),
                    Finish::Refused,
                    Some(map(json!({ "why": "Name a commit." })))
                ),
                (
                    "dr-2".to_string(),
                    Finish::Failed,
                    Some(map(json!({ "why": "git exploded" })))
                ),
            ]
        );
    });
}

#[tokio::test(start_paused = true)]
async fn a_run_that_ends_failed_keeps_its_result() {
    let fake = Fake::with(vec![request("dr-1", DeployKind::TestflightBuild)]);
    fake.state(|s| {
        s.runs.push_back(Ok(Outcome {
            status: Finish::Failed,
            result: map(json!({ "sha": "abc1234", "buildNumber": 7, "result": "build_failed" })),
        }));
    });
    DeployRequests::new(fake.clone()).cycle().await;
    settle().await;
    fake.state(|s| {
        assert_eq!(s.finished[0].1, Finish::Failed);
        assert_eq!(s.finished[0].2.as_ref().unwrap()["buildNumber"], 7);
    });
}

#[tokio::test(start_paused = true)]
async fn a_result_over_four_kb_is_failed_with_a_short_reason() {
    let fake = Fake::with(vec![request("dr-1", DeployKind::Hosting)]);
    fake.state(|s| s.runs.push_back(Ok(done(map(json!({ "out": "x".repeat(5000) }))))));
    DeployRequests::new(fake.clone()).cycle().await;
    settle().await;
    fake.state(|s| {
        assert_eq!(s.finished[0].1, Finish::Failed);
        let why = s.finished[0].2.as_ref().unwrap()["why"].as_str().unwrap().to_string();
        assert!(why.contains("4 KB"), "{why}");
    });
}

#[tokio::test(start_paused = true)]
async fn a_long_reason_is_cut() {
    let fake = Fake::with(vec![request("dr-1", DeployKind::Promote)]);
    fake.state(|s| s.runs.push_back(Err(RunError::Failed("é".repeat(2000)))));
    DeployRequests::new(fake.clone()).cycle().await;
    settle().await;
    let why = fake.state(|s| {
        s.finished[0].2.as_ref().unwrap()["why"]
            .as_str()
            .unwrap()
            .chars()
            .count()
    });
    assert_eq!(why, WHY_MAX);
}

#[tokio::test(start_paused = true)]
async fn losing_the_claim_runs_nothing() {
    let fake = Fake::with(vec![request("dr-1", DeployKind::Hosting)]);
    fake.state(|s| s.lose = true);
    let mut loop_ = DeployRequests::new(fake.clone());
    loop_.cycle().await;
    settle().await;
    fake.state(|s| {
        assert_eq!(s.claims, ["dr-1"]);
        assert!(s.ran.is_empty());
        assert!(s.finished.is_empty());
    });
    assert_eq!(loop_.running(), 0);
}

#[tokio::test(start_paused = true)]
async fn only_open_requests_are_claimed() {
    let mut rows = Vec::new();
    for (id, status) in [
        ("a", DeployStatus::Claimed),
        ("b", DeployStatus::Running),
        ("c", DeployStatus::Done),
        ("d", DeployStatus::Refused),
        ("e", DeployStatus::Failed),
        ("f", DeployStatus::Requested),
    ] {
        let mut r = request(id, DeployKind::Hosting);
        r.status = status;
        rows.push(r);
    }
    let fake = Fake::with(rows);
    DeployRequests::new(fake.clone()).cycle().await;
    settle().await;
    assert_eq!(fake.state(|s| s.claims.clone()), ["f"]);
}

#[tokio::test(start_paused = true)]
async fn a_request_preferring_another_computer_waits_five_seconds_for_it() {
    let prefer = |id: &str, who: &str, age_ms: u64| {
        let mut r = request(id, DeployKind::TestflightBuild);
        r.args.prefer = Some(who.into());
        r.at = NOW - age_ms;
        r
    };
    let fake = Fake::with(vec![
        prefer("fresh-other", "b-pc", 1_000),
        prefer("old-other", "b-pc", PREFER_WAIT_MS),
        prefer("fresh-mine", "a-pc", 1_000),
    ]);
    DeployRequests::new(fake.clone()).cycle().await;
    settle().await;
    assert_eq!(fake.state(|s| s.claims.clone()), ["old-other", "fresh-mine"]);
}

#[tokio::test(start_paused = true)]
async fn a_request_being_run_is_not_taken_again_and_a_long_run_beats_every_minute() {
    let fake = Fake::with(vec![request("dr-1", DeployKind::TestflightBuild)]);
    fake.state(|s| s.run_for = Duration::from_secs(150));
    let mut loop_ = DeployRequests::new(fake.clone());
    loop_.cycle().await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    // The store still lists it open (a late push): this computer holds it and does not claim it twice.
    loop_.cycle().await;
    assert_eq!(fake.state(|s| s.claims.clone()), ["dr-1"]);
    assert_eq!(loop_.running(), 1);
    tokio::time::sleep(Duration::from_secs(150)).await;
    fake.state(|s| {
        assert_eq!(s.beats, 3, "once at the start, then at 60 and 120 seconds");
        assert_eq!(s.finished.len(), 1);
    });
    assert_eq!(loop_.running(), 0);
}

#[tokio::test(start_paused = true)]
async fn an_unreadable_store_claims_nothing() {
    let fake = Fake::with(vec![request("dr-1", DeployKind::Hosting)]);
    fake.state(|s| s.list_error = true);
    assert_eq!(DeployRequests::new(fake.clone()).cycle().await, WAIT);
    settle().await;
    assert!(fake.state(|s| s.claims.is_empty()));
}

struct Keys(Option<String>);

impl crate::link::TokenStore for Keys {
    fn get(&self, _: &str) -> anyhow::Result<Option<String>> {
        Ok(self.0.clone())
    }
    fn set(&self, _: &str, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
    fn delete(&self, _: &str) -> anyhow::Result<()> {
        Ok(())
    }
}

fn live(base: &str, token: Option<&str>) -> Live {
    Live {
        project: ProjectConfig {
            id: Some("proj".into()),
            github_repo: "ann/site".into(),
            repo_path: "site".into(),
            ..Default::default()
        },
        machine: "a-pc".into(),
        backlog_url: base.into(),
        keychain: Arc::new(Keys(token.map(String::from))),
        client: crate::backlog::Client::new(base),
        repo_id: tokio::sync::Mutex::new(Some(5)),
        paused: Default::default(),
        store_paused: Default::default(),
    }
}

#[tokio::test]
async fn the_live_loop_sends_the_stores_ops_for_this_project() {
    let (base, seen) = crate::backlog::tests::store(200, json!({ "value": [] })).await;
    let l = live(&base, Some("tok"));
    assert!(l.list().await.unwrap().is_empty());
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "deploy_list");
}

#[tokio::test]
async fn without_a_link_to_the_store_nothing_is_read_or_claimed() {
    let (base, seen) = crate::backlog::tests::store(200, json!({ "value": [] })).await;
    let l = live(&base, None);
    assert!(l.list().await.unwrap_err().contains("no link"));
    assert!(l.claim("dr-1").await.is_err());
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn only_the_ported_kinds_are_claimable() {
    let l = live("http://127.0.0.1:1", Some("tok"));
    for kind in DeployKind::ALL {
        assert_eq!(l.can(&request("dr-1", kind)).await, PORTED.contains(&kind), "{kind:?}");
    }
    assert_eq!(
        PORTED,
        [DeployKind::Hosting],
        "a kind joins with its arm in Live::run and its tests"
    );
}

#[tokio::test]
async fn hosting_needs_only_the_project_and_a_link_to_the_store() {
    let hosting = request("dr-1", DeployKind::Hosting);
    assert!(live("http://127.0.0.1:1", Some("tok")).can(&hosting).await);
    // No link to the store: nothing to report to, so it is left to a computer that has one.
    assert!(!live("http://127.0.0.1:1", None).can(&hosting).await);
    // No repo to read.
    let mut bare = live("http://127.0.0.1:1", Some("tok"));
    bare.project.repo_path.clear();
    assert!(!bare.can(&hosting).await);
    // A paused project still reads.
    let paused = live("http://127.0.0.1:1", Some("tok"));
    paused.paused.set("proj", Some("limit".into()));
    assert!(paused.can(&hosting).await);
}

#[tokio::test]
async fn a_hosting_request_answers_the_scan_and_the_pick_and_writes_nothing() {
    let (base, seen) = crate::backlog::tests::store(200, json!({ "value": {} })).await;
    let mut l = live(&base, Some("tok"));
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Dockerfile"), "FROM node\nEXPOSE 8080\n").unwrap();
    l.project.repo_path = dir.path().to_string_lossy().into_owned();
    // No repo name for `gh` to look up: visibility unknown, so not public.
    l.project.github_repo.clear();
    l.project.project_type = crate::config::ProjectType::Web;
    let out = l.run(&request("dr-1", DeployKind::Hosting)).await.unwrap();
    assert_eq!(out.status, Finish::Done);
    assert_eq!(
        Value::Object(checked_result(out.result).unwrap()),
        json!({
            "project": { "type": "web", "public": false, "backEnd": true, "app": null },
            "pick": {
                "shape": "backEnd",
                "default": "cloudflare-workers",
                "alternatives": ["render", "cloud-run"],
                "database": { "default": "cloudflare-d1", "alternatives": ["supabase", "neon", "turso"] },
            },
        })
    );
    assert!(seen.lock().unwrap().is_empty(), "a read sends the store nothing");
}

#[test]
fn a_paused_project_builds_and_promotes_nothing_but_still_reads() {
    use crate::view::StorePause;
    let l = live("http://127.0.0.1:1", Some("tok"));
    assert!(!l.paused());
    l.paused.set("proj", Some("limit".into()));
    assert!(l.paused());
    l.paused.set("proj", None);
    l.store_paused.set("proj", StorePause::Paused("holiday".into()));
    assert!(l.paused());
    let held: Vec<_> = DeployKind::ALL.into_iter().filter(|k| waits_while_paused(*k)).collect();
    assert_eq!(held, [DeployKind::TestflightBuild, DeployKind::Promote]);
}
