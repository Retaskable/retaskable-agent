//! The rollout loop against fakes of the store, the shell, git and the clock (tokio's paused time).
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::*;
use crate::backlog::{Item, ItemStatus};
use crate::config::{DeployConfig, DeployTarget, RolloutConfig};

const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

#[derive(Default)]
struct State {
    answers: VecDeque<Result<Value, String>>,
    polled: Vec<Option<Value>>,
    ran: Vec<String>,
    outs: VecDeque<Cmd>,
    run_for: Duration,
    beats: usize,
    beat_held: bool,
    reports: Vec<Value>,
    git_fails: bool,
    items: Vec<Item>,
    ancestors: Vec<(String, String)>,
    served: Option<String>,
    production_served: Option<String>,
    /// `gh api` answers by path suffix; a path with none is an error.
    gh: Vec<(String, Value)>,
    gh_paths: Vec<String>,
    log: String,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<State>>);

impl Deps for Fake {
    async fn poll(&self, result: Option<Value>) -> Result<Value, String> {
        let mut s = self.0.lock().unwrap();
        s.polled.push(result);
        s.answers.pop_front().unwrap_or(Ok(json!({ "idle": true })))
    }
    async fn beat(&self, _: Value) -> Result<Value, String> {
        let mut s = self.0.lock().unwrap();
        s.beats += 1;
        Ok(json!({ "held": s.beat_held }))
    }
    async fn report(&self, report: Value) -> Result<(), String> {
        self.0.lock().unwrap().reports.push(report);
        Ok(())
    }
    async fn run(&self, command: String, _: Duration) -> Cmd {
        let (wait, out) = {
            let mut s = self.0.lock().unwrap();
            s.ran.push(command);
            (s.run_for, s.outs.pop_front().unwrap_or_default())
        };
        tokio::time::sleep(wait).await;
        out
    }
    async fn fetch(&self) -> Result<(), String> {
        Ok(())
    }
    async fn git(&self, args: Vec<String>) -> Result<String, String> {
        let s = self.0.lock().unwrap();
        if s.git_fails {
            return Err("git failed".into());
        }
        Ok(match args[0].as_str() {
            "rev-parse" => "p".repeat(40),
            _ => (0..250).map(|i| format!("{i:040x} {}\n", 1000 + i)).collect(),
        })
    }
    async fn is_ancestor(&self, a: String, b: String) -> Result<bool, String> {
        Ok(a == b || self.0.lock().unwrap().ancestors.contains(&(a, b)))
    }
    async fn items(&self) -> Result<Vec<Item>, String> {
        Ok(self.0.lock().unwrap().items.clone())
    }
    async fn staging_served(&self) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap().served.clone())
    }
    async fn production_served(&self) -> Result<String, String> {
        self.0
            .lock()
            .unwrap()
            .production_served
            .clone()
            .ok_or("no address".into())
    }
    async fn gh_api(&self, path: String) -> Result<Value, String> {
        let mut s = self.0.lock().unwrap();
        s.gh_paths.push(path.clone());
        s.gh.iter()
            .find(|(p, _)| *p == path)
            .map(|(_, v)| v.clone())
            .ok_or("403".into())
    }
}

fn cfg(staging: bool, rollout: Option<(&str, &str)>) -> ProjectConfig {
    let target = |branch: &str| DeployTarget {
        branch: branch.into(),
        url: None,
        smoke: None,
        version_path: None,
        rollout: rollout.map(|(t, h)| RolloutConfig {
            traffic: Some(t.into()),
            health: Some(h.into()),
        }),
    };
    ProjectConfig {
        deploy: DeployConfig {
            staging: staging.then(|| target("staging")),
            production: Some(target("main")),
        },
        ..Default::default()
    }
}

fn rollout(fake: &Fake) -> Rollout<Fake> {
    let log = fake.0.clone();
    Rollout::new(
        fake.clone(),
        &cfg(true, Some(("t", "h"))),
        Box::new(move |l| {
            let mut s = log.lock().unwrap();
            s.log.push_str(l);
            s.log.push('\n');
        }),
    )
    .unwrap()
}

fn tell(fake: &Fake, answer: Value) {
    fake.0.lock().unwrap().answers.push_back(Ok(answer));
}

fn order(id: &str, command: &str, sha: &str, percent: Option<u64>) -> Value {
    json!({ "do": { "id": id, "command": command, "sha": sha, "percent": percent }, "afterMs": 0 })
}

#[tokio::test(start_paused = true)]
async fn idles_and_logs_why_once() {
    let f = Fake::default();
    let mut r = rollout(&f);
    for _ in 0..2 {
        tell(&f, json!({ "idle": true, "why": "no report yet", "afterMs": 2000 }));
        assert_eq!(r.step().await, Duration::from_secs(2));
    }
    assert_eq!(f.0.lock().unwrap().log, "idle: no report yet\n");
    assert!(f.0.lock().unwrap().ran.is_empty());
}

#[tokio::test(start_paused = true)]
async fn traffic_runs_once_and_the_result_goes_on_the_next_poll() {
    let f = Fake::default();
    let mut r = rollout(&f);
    tell(&f, order("a", "traffic", SHA, Some(25)));
    assert_eq!(r.step().await, Duration::ZERO);
    tell(&f, order("a", "traffic", SHA, Some(25)));
    assert_eq!(r.step().await, WAIT);
    let s = f.0.lock().unwrap();
    assert_eq!(s.ran, ["t 25"]);
    assert_eq!(s.polled[1], Some(json!({ "id": "a", "ok": true })));
    assert!(s.log.contains("told to traffic 25 012345678\ntraffic 012345678: ok\n"));
}

#[tokio::test(start_paused = true)]
async fn bad_sha_and_bad_percent_fail_without_running() {
    let f = Fake::default();
    let mut r = rollout(&f);
    tell(&f, order("a", "traffic", "abc", Some(25)));
    r.step().await;
    assert_eq!(r.kept.as_ref().unwrap().1["why"], "that isn't a commit");
    tell(&f, order("b", "traffic", SHA, Some(101)));
    r.step().await;
    assert_eq!(
        r.kept.as_ref().unwrap().1["why"],
        "no traffic command, or a bad percent"
    );
    tell(&f, order("c", "dance", SHA, None));
    r.step().await;
    assert_eq!(r.kept.as_ref().unwrap().1["why"], "unknown instruction dance");
    tell(&f, order("d", "promote", SHA, None));
    r.step().await;
    assert_eq!(r.kept.as_ref().unwrap().1["why"], "this computer can't promote yet");
    assert!(f.0.lock().unwrap().ran.is_empty());
}

#[tokio::test(start_paused = true)]
async fn health_sends_its_last_line_and_is_rate_limited() {
    let f = Fake::default();
    f.0.lock().unwrap().outs.push_back(Cmd {
        code: 0,
        out: "x\nall well\n\n".into(),
        timed_out: false,
    });
    let mut r = rollout(&f);
    tell(&f, order("a", "health", SHA, None));
    r.step().await;
    assert_eq!(
        r.kept.as_ref().unwrap().1,
        json!({ "id": "a", "ok": true, "line": "all well" })
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    tell(&f, order("b", "health", SHA, None));
    let wait = r.step().await;
    assert_eq!(wait, Duration::from_secs(6));
    assert_eq!(f.0.lock().unwrap().ran.len(), 1);
    assert!(
        f.0.lock()
            .unwrap()
            .log
            .contains("health asked again too soon, waits 7s")
    );
    tokio::time::sleep(wait).await;
    tell(&f, order("b", "health", SHA, None));
    r.step().await;
    assert_eq!(f.0.lock().unwrap().ran.len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_failure_says_the_last_line_or_the_exit_code() {
    let f = Fake::default();
    f.0.lock().unwrap().outs.extend([
        Cmd {
            code: 2,
            out: "boom\n".into(),
            timed_out: false,
        },
        Cmd {
            code: 3,
            out: String::new(),
            timed_out: false,
        },
        Cmd {
            code: -1,
            out: String::new(),
            timed_out: true,
        },
    ]);
    let mut r = rollout(&f);
    for (id, why) in [("a", "boom"), ("b", "exit 3"), ("c", "timed out")] {
        tell(&f, order(id, "traffic", SHA, Some(0)));
        r.step().await;
        assert_eq!(r.kept.as_ref().unwrap().1["why"], why);
    }
}

#[tokio::test(start_paused = true)]
async fn a_beat_that_says_not_held_drops_the_result() {
    let f = Fake::default();
    f.0.lock().unwrap().run_for = Duration::from_secs(40);
    let mut r = rollout(&f);
    tell(&f, order("a", "traffic", SHA, Some(5)));
    assert_eq!(r.step().await, WAIT);
    assert!(r.kept.is_none());
    assert_eq!(f.0.lock().unwrap().beats, 1);
    assert!(f.0.lock().unwrap().log.contains("no longer the runner, result dropped"));
}

#[tokio::test(start_paused = true)]
async fn a_long_command_beats_every_fifteen_seconds() {
    let f = Fake::default();
    {
        let mut s = f.0.lock().unwrap();
        s.run_for = Duration::from_secs(41);
        s.beat_held = true;
    }
    let mut r = rollout(&f);
    tell(&f, order("a", "traffic", SHA, Some(5)));
    r.step().await;
    assert_eq!(f.0.lock().unwrap().beats, 3); // at 10, 25 and 40 seconds
    assert!(r.kept.is_some());
}

#[tokio::test(start_paused = true)]
async fn a_store_error_or_not_held_runs_nothing() {
    let f = Fake::default();
    let mut r = rollout(&f);
    f.0.lock().unwrap().answers.push_back(Err("down".into()));
    assert_eq!(r.step().await, WAIT);
    tell(&f, json!({ "held": false }));
    assert_eq!(r.step().await, WAIT);
    let s = f.0.lock().unwrap();
    assert!(s.ran.is_empty());
    assert_eq!(s.log, "poll error: down\nanother computer runs the rollout\n");
}

#[test]
fn it_applies_only_with_both_commands_and_a_branch() {
    assert!(uses_store(&cfg(true, Some(("t", "h")))).is_ok());
    assert!(uses_store(&cfg(false, Some(("t", "h")))).is_ok());
    assert!(uses_store(&cfg(true, Some(("t", " ")))).is_err());
    assert!(uses_store(&cfg(true, None)).is_err());
    let site: ProjectConfig = serde_json::from_value(json!({
        "deploy": { "production": { "branch": "main", "rollout": { "site": true } } }
    }))
    .unwrap();
    assert!(uses_store(&site).is_err());
}

fn item(id: &str, status: ItemStatus, merge: &str) -> Item {
    Item {
        id: id.into(),
        status,
        merge_commit: Some(merge.into()),
        ..Default::default()
    }
}

#[tokio::test(start_paused = true)]
async fn the_report_cuts_at_200_and_names_items_and_served() {
    let f = Fake::default();
    let newest = format!("{:040x}", 0);
    let older = format!("{:040x}", 1);
    {
        let mut s = f.0.lock().unwrap();
        s.items = vec![
            item("one", ItemStatus::OnBranch, &older),
            item("two", ItemStatus::Todo, &newest),
        ];
        s.ancestors.push((older.clone(), newest.clone()));
        s.served = Some(older.clone());
        s.ancestors.push((older.clone(), format!("{:040x}", 5)));
    }
    let report = report::build(&f, "main", Some("staging")).await.unwrap();
    let commits = report["commits"].as_array().unwrap();
    assert_eq!(commits.len(), 200);
    assert_eq!(report["production"], "p".repeat(40));
    assert_eq!(
        commits[0],
        json!({ "sha": newest, "at": 1_000_000, "items": ["one"], "served": false })
    );
    assert_eq!(commits[1]["served"], true);
}

#[tokio::test(start_paused = true)]
async fn the_report_is_sent_when_it_changes_and_not_when_git_fails() {
    let f = Fake::default();
    let mut r = rollout(&f);
    f.0.lock().unwrap().git_fails = true;
    r.cycle().await;
    assert!(f.0.lock().unwrap().reports.is_empty());
    assert!(f.0.lock().unwrap().log.contains("nothing reported: git failed"));
    f.0.lock().unwrap().git_fails = false;
    tokio::time::sleep(REPORT_EVERY).await;
    r.cycle().await;
    tokio::time::sleep(REPORT_EVERY).await;
    r.cycle().await;
    assert_eq!(f.0.lock().unwrap().reports.len(), 1);
}

fn runs(conclusions: &[(&str, Option<&str>)]) -> Value {
    json!({ "check_runs": conclusions.iter().map(|(s, c)| json!({ "status": s, "conclusion": c })).collect::<Vec<_>>() })
}

fn production_only(f: &Fake) -> Rollout<Fake> {
    let log = f.0.clone();
    Rollout::new(
        f.clone(),
        &cfg(false, Some(("t", "h"))),
        Box::new(move |l| log.lock().unwrap().log.push_str(&format!("{l}\n"))),
    )
    .unwrap()
}

#[tokio::test(start_paused = true)]
async fn production_only_serves_up_to_the_newest_passing_commit() {
    let f = Fake::default();
    let sha = |i: usize| format!("{i:040x}");
    {
        let mut s = f.0.lock().unwrap();
        s.production_served = Some("a".repeat(40));
        s.gh = vec![
            (format!("commits/{}/check-runs", sha(0)), runs(&[("in_progress", None)])),
            (
                format!("commits/{}/check-runs", sha(1)),
                runs(&[("completed", Some("success")), ("queued", None)]),
            ),
            (
                format!("commits/{}/check-runs", sha(2)),
                runs(&[("completed", Some("success"))]),
            ),
        ];
        s.served = Some(sha(0)); // a leftover smoke is not read
    }
    let report = report::build(&f, "main", None).await.unwrap();
    assert_eq!(report["production"], "a".repeat(40));
    let commits = report["commits"].as_array().unwrap();
    let served: Vec<_> = commits.iter().take(4).map(|c| c["served"].clone()).collect();
    assert_eq!(served, [json!(false), json!(false), json!(true), json!(true)]);
    assert!(f.0.lock().unwrap().gh_paths.iter().all(|p| p.ends_with("check-runs")));
}

#[tokio::test(start_paused = true)]
async fn production_only_reads_statuses_only_when_check_runs_cant_be_read() {
    let f = Fake::default();
    {
        let mut s = f.0.lock().unwrap();
        s.production_served = Some("a".repeat(40));
        s.gh = vec![(
            format!("commits/{:040x}/status", 0),
            json!({ "state": "success", "statuses": [{ "state": "success" }] }),
        )];
    }
    let report = report::build(&f, "main", None).await.unwrap();
    assert_eq!(report["commits"][0]["served"], true);
    let paths = f.0.lock().unwrap().gh_paths.clone();
    assert_eq!(paths[0], format!("commits/{:040x}/check-runs", 0));
    assert_eq!(paths[1], format!("commits/{:040x}/status", 0));
    // Nothing is reported while production's commit can't be read, and a commit with no checks serves nothing.
    f.0.lock().unwrap().production_served = None;
    assert!(report::build(&f, "main", None).await.is_err());
    f.0.lock().unwrap().production_served = Some("a".repeat(40));
    f.0.lock().unwrap().gh = vec![(format!("commits/{:040x}/check-runs", 0), runs(&[]))];
    let report = report::build(&f, "main", None).await.unwrap();
    assert!(
        report["commits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["served"] == false)
    );
}

#[tokio::test(start_paused = true)]
async fn production_only_promote_succeeds_and_runs_nothing() {
    let f = Fake::default();
    let mut r = production_only(&f);
    tell(&f, order("a", "promote", SHA, None));
    r.step().await;
    assert_eq!(r.kept.as_ref().unwrap().1, json!({ "id": "a", "ok": true }));
    assert!(f.0.lock().unwrap().ran.is_empty());
}

#[test]
fn the_agent_keeps_no_rollout_of_its_own() {
    fn walk(dir: &std::path::Path, found: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, found);
            } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("rollout/tests.rs") {
                let text = std::fs::read_to_string(&p).unwrap();
                for word in ["step_index", "hold_until", "release_resume", "rollout_stop"] {
                    if text.contains(word) {
                        found.push(format!("{}: {word}", p.display()));
                    }
                }
            }
        }
    }
    let mut found = Vec::new();
    walk(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut found,
    );
    assert!(found.is_empty(), "{found:?}");
}
