//! The pass against a fake store (a real [`Client`] over HTTP, a scripted `ci_result`), a fake `gh` (a shell script
//! over files) and a fake judge engine (the real [`ci_judge::judge`] over a canned session result).
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

use super::*;
use crate::engines::engine::{test_config, test_result};
use crate::test_support::write_executable;

mod answers;
mod checks;
mod fix;
mod gitlab;
mod merge;
mod press;
mod rerun;

pub(super) type Script = Box<dyn Fn(&Value) -> Value + Send + Sync>;

/// A fake store: records every op and answers `ci_result` from the script (default: green, nothing to do).
pub(super) struct Store {
    pub url: String,
    pub ops: Arc<Mutex<Vec<(String, Value)>>>,
    script: Arc<Mutex<Script>>,
    down: Arc<Mutex<bool>>,
}

impl Store {
    pub async fn start() -> Self {
        let ops: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let script: Arc<Mutex<Script>> = Arc::new(Mutex::new(Box::new(|_| green())));
        let down = Arc::new(Mutex::new(false));
        type Shared = (Arc<Mutex<Vec<(String, Value)>>>, Arc<Mutex<Script>>, Arc<Mutex<bool>>);
        let handler = |State((ops, script, down)): State<Shared>, Json(body): Json<Value>| async move {
            let op = body["op"].as_str().unwrap_or("").to_string();
            let args = body["args"].clone();
            ops.lock().unwrap().push((op.clone(), args.clone()));
            if *down.lock().unwrap() {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({ "error": "boom" })),
                );
            }
            let value = if op == "ci_result" {
                (script.lock().unwrap())(&args)
            } else {
                json!({ "ok": true })
            };
            (axum::http::StatusCode::OK, Json(json!({ "value": value })))
        };
        let app = Router::new().route("/v1/repos/{id}/ops", post(handler)).with_state((
            ops.clone(),
            script.clone(),
            down.clone(),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, ops, script, down }
    }

    pub fn answers(&self, f: impl Fn(&Value) -> Value + Send + Sync + 'static) {
        *self.script.lock().unwrap() = Box::new(f);
    }

    pub fn go_down(&self) {
        *self.down.lock().unwrap() = true;
    }

    /// The args of each call of `op`.
    pub fn sent(&self, op: &str) -> Vec<Value> {
        self.ops
            .lock()
            .unwrap()
            .iter()
            .filter(|(o, _)| o == op)
            .map(|(_, a)| a.clone())
            .collect()
    }
}

pub(super) fn green() -> Value {
    json!({ "ok": true, "condition": "green", "next": null })
}

/// An answer with `next` (and what to write and show).
pub(super) fn tell(next: Value) -> Value {
    json!({ "ok": true, "condition": "red", "found": true, "next": next })
}

/// A fake `gh` over files in its folder: `pr list` prints `list.json`; a commit's statuses and check runs are
/// `<sha>.status` and `<sha>.runs` (no file, no answer: `gh` fails); `pr checks <n>` prints `checks-<n>.json`;
/// `run view` prints a log; `api user` prints `login` (no file: `gh` fails, as when signed out); a `POST` to the labels fails with "Not Found" until the label is made, when `no-label`
/// is there. Every call is appended to `gh.log`.
pub(super) fn fake_gh(dir: &Path) -> PathBuf {
    let d = dir.display();
    let script = format!(
        r#"#!/bin/sh
[ -n "$1" ] || exit 0
echo "$*" >> "{d}/gh.log"
case "$*" in
  "pr list"*) cat "{d}/list.json" ;;
  "api repos/o/r/commits/"*"/status?"*) f="{d}/$(echo "$2" | cut -d/ -f5).status"; [ -f "$f" ] && cat "$f" || {{ echo "HTTP 403" >&2; exit 1; }} ;;
  "api repos/o/r/commits/"*"/check-runs?"*) f="{d}/$(echo "$2" | cut -d/ -f5).runs"; [ -f "$f" ] && cat "$f" ;;
  "api user"*) [ -f "{d}/login" ] && cat "{d}/login" || exit 1 ;;
  "pr checks "*) cat "{d}/checks-$3.json" ;;
  "run view"*) echo "error: boom in the build" ;;
  "api -X POST"*) if [ -f "{d}/no-label" ] && [ ! -f "{d}/label-made" ]; then echo "Not Found" >&2; exit 1; fi ;;
  "label create"*) touch "{d}/label-made" ;;
esac
exit 0
"#
    );
    let gh = dir.join("gh");
    write_executable(&gh, &script);
    gh
}

pub(super) fn pull(number: u64, branch: &str, head: &str) -> Value {
    json!({ "number": number, "title": format!("Title {number}"), "url": format!("https://github.com/o/r/pull/{number}"),
        "headRefName": branch, "headRefOid": head, "mergeable": "MERGEABLE", "author": { "login": "ann" },
        "isCrossRepository": false })
}

pub(super) fn run_json(name: &str, status: &str, conclusion: Option<&str>) -> Value {
    json!({ "name": name, "status": status, "conclusion": conclusion,
        "html_url": format!("https://github.com/o/r/actions/runs/42/job/{name}"), "completed_at": "2026-11-01T10:05:00Z" })
}

/// A head's check runs, as `gh api ... --jq '.check_runs[]'` prints them: one object per line.
pub(super) fn head_is(dir: &Path, sha: &str, runs: Vec<Value>) {
    std::fs::write(dir.join(format!("{sha}.status")), "").unwrap();
    let lines: Vec<String> = runs.iter().map(Value::to_string).collect();
    std::fs::write(dir.join(format!("{sha}.runs")), lines.join("\n")).unwrap();
}

pub(super) fn red(dir: &Path, sha: &str) {
    head_is(dir, sha, vec![run_json("build", "completed", Some("failure"))]);
}

pub(super) fn list(dir: &Path, pulls: Vec<Value>) {
    std::fs::write(dir.join("list.json"), Value::Array(pulls).to_string()).unwrap();
}

/// What the judge was asked: pull request, title, branch and the failed checks' note.
type Asked = (u64, String, String, String);

/// The judge engine, faked: the real [`ci_judge::judge`] over a canned session result.
pub(super) struct FakeJudge {
    pub asked: Mutex<Vec<Asked>>,
    pub says: Option<(CiVerdict, &'static str)>,
}

impl Judge for FakeJudge {
    fn judge<'a>(
        &'a self,
        pr: u64,
        title: &'a str,
        branch: &'a str,
        what: &'a str,
    ) -> BoxFuture<'a, Option<(CiVerdict, String)>> {
        Box::pin(async move {
            self.asked
                .lock()
                .unwrap()
                .push((pr, title.into(), branch.into(), what.into()));
            let structured = self.says.map(|(v, why)| json!({ "verdict": v, "why": why }));
            ci_judge::judge(pr, title, branch, what, &test_config(), |_| async move {
                Ok(test_result(structured.is_some(), "success", structured, None))
            })
            .await
        })
    }
}

/// What the fix session was asked: pull request, title, branch, the failed checks' note and the worktree.
type FixAsked = (u64, String, String, String, PathBuf);

/// What the fix session did in its worktree, and whether it went ok.
type Acts = Box<dyn Fn(&Path) -> bool + Send + Sync>;

/// The fix session, faked: `acts` runs in the worktree (a fake engine that commits there, say). Default: no change, ok.
pub(super) struct FakeFixer {
    pub asked: Mutex<Vec<FixAsked>>,
    pub acts: Mutex<Acts>,
}

impl Fixer for FakeFixer {
    fn fix<'a>(
        &'a self,
        pr: u64,
        title: &'a str,
        branch: &'a str,
        what: &'a str,
        dir: &'a Path,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.asked
                .lock()
                .unwrap()
                .push((pr, title.into(), branch.into(), what.into(), dir.to_path_buf()));
            (self.acts.lock().unwrap())(dir)
        })
    }
}

pub(super) struct Rig {
    pub dir: tempfile::TempDir,
    pub store: Store,
    pub judge: FakeJudge,
    pub fixer: FakeFixer,
    pub bells: Mutex<Vec<(String, String)>>,
    pub client: Client,
    pub items: HashSet<String>,
    pub ready_label: bool,
    /// The project's code is on GitLab (its `codeHost`), so `host()` is `host_for`'s.
    pub gitlab: bool,
    /// The project's `ciWatchAllow`.
    pub allow: Vec<String>,
    pub logins: LoginCache,
    /// The heads something is under way on.
    pub fixing: Fixing,
    /// The pass holds its write steps.
    pub hold_writes: bool,
    /// The CI grace, in ms.
    pub ci_grace_ms: u64,
    /// The checkout `gh` and git run in; the rig's own folder until a test makes a repo.
    pub repo: Option<PathBuf>,
    pub git: host::GitCommand,
    /// `with_repo`'s branch tip: the sha a fix is judged at.
    pub tip: String,
}

impl Rig {
    pub async fn new() -> Self {
        let store = Store::start().await;
        Self {
            dir: tempfile::tempdir().unwrap(),
            client: Client::new(&store.url),
            store,
            judge: FakeJudge {
                asked: Mutex::default(),
                says: Some((CiVerdict::NotChange, "the machine failed")),
            },
            fixer: FakeFixer {
                asked: Mutex::default(),
                acts: Mutex::new(Box::new(|_| true)),
            },
            bells: Mutex::default(),
            items: HashSet::new(),
            ready_label: false,
            gitlab: false,
            allow: Vec::new(),
            logins: LoginCache::default(),
            fixing: Fixing::default(),
            hold_writes: false,
            ci_grace_ms: 10 * 60_000,
            repo: None,
            git: Arc::new(crate::test_support::git),
            tip: String::new(),
        }
    }

    pub fn host(&self) -> Arc<dyn CiHost> {
        if self.gitlab {
            let project = crate::config::ProjectConfig {
                code_host: Some(crate::config::CodeHostConfig { kind: "gitlab".into() }),
                ..Default::default()
            };
            return host_for(&project, fake_gh(self.dir.path()));
        }
        Arc::new(GhCiHost {
            gh: fake_gh(self.dir.path()),
            dir: self.repo.clone().unwrap_or_else(|| self.dir.path().to_path_buf()),
            repo: "o/r".into(),
            base: "main".into(),
            ready_label: self.ready_label,
            worktrees: self.dir.path().join("worktrees"),
            git: self.git.clone(),
        })
    }

    /// One pass at `now`, with `since` kept by the caller.
    pub async fn pass(&self, since: &mut HashMap<String, u64>, now: u64) -> Result<u32> {
        let bell = |t: &str, b: &str| self.bells.lock().unwrap().push((t.into(), b.into()));
        Pass {
            host: self.host(),
            judge: &self.judge,
            fixer: &self.fixer,
            client: &self.client,
            repo: 7,
            token: Some("tok"),
            items: &self.items,
            allow: &self.allow,
            fixing: &self.fixing,
            ci_grace_ms: self.ci_grace_ms,
            hold_writes: self.hold_writes,
            logins: &self.logins,
            bell: &bell,
            now,
        }
        .run(since)
        .await
    }

    pub fn gh_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("gh.log"))
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect()
    }

    /// The runs `gh` was told to cancel or start again.
    pub fn run_calls(&self) -> Vec<String> {
        let mut calls = self.gh_log();
        calls.retain(|l| l.starts_with("run cancel") || l.starts_with("run rerun"));
        calls
    }

    pub fn judged(&self) -> Vec<Asked> {
        self.judge.asked.lock().unwrap().clone()
    }
}

#[tokio::test]
async fn each_head_is_sent_with_its_facts_and_may_push_by_the_rule() {
    let rig = Rig::new().await;
    list(
        rig.dir.path(),
        vec![
            pull(1, "feat/a", "aaa"),
            pull(2, "retaskable/2-b", "bbb"),
            pull(3, "feat/c", "ccc"),
        ],
    );
    head_is(
        rig.dir.path(),
        "aaa",
        vec![run_json("build", "completed", Some("success"))],
    );
    red(rig.dir.path(), "bbb");
    head_is(rig.dir.path(), "ccc", vec![run_json("build", "in_progress", None)]);
    rig.store
        .answers(|a| json!({ "ok": true, "condition": "green", "found": a["pr"] == 2, "next": null }));
    let found = rig.pass(&mut HashMap::new(), 5_000).await.unwrap();
    assert_eq!(found, 1);
    let sent = rig.store.sent("ci_result");
    assert_eq!(sent.len(), 3);
    // Only the `retaskable/` branch may be pushed to: ann isn't signed in, allowed or the author's own.
    for (facts, may_push) in sent.iter().zip([false, true, false]) {
        assert_eq!(
            (&facts["mayPush"], &facts["button"]),
            (&json!(may_push), &json!(false)),
            "{facts}"
        );
        assert_eq!(facts["redNow"], 1);
        assert!(facts.get("baseHead").is_none() && facts.get("did").is_none(), "{facts}");
    }
    assert_eq!(sent[0]["checks"]["state"], "pass");
    assert_eq!(
        (&sent[0]["title"], &sent[0]["branch"], &sent[0]["sha"]),
        (&json!("Title 1"), &json!("feat/a"), &json!("aaa"))
    );
    assert_eq!(sent[0]["conflicting"], false);
    assert_eq!(sent[1]["checks"]["state"], "fail");
    assert!(
        sent[1]["checks"]["what"]
            .as_str()
            .unwrap()
            .contains("boom in the build")
    );
    assert!(sent[1]["checks"]["endedAt"].is_number());
    assert_eq!(
        (&sent[2]["checks"]["state"], &sent[2]["pendingSince"]),
        (&json!("pending"), &json!(5_000))
    );
}

#[tokio::test]
async fn a_head_is_pending_since_the_first_pass_that_saw_it_until_it_passes_or_moves() {
    let rig = Rig::new().await;
    list(rig.dir.path(), vec![pull(1, "feat/a", "aaa")]);
    head_is(rig.dir.path(), "aaa", vec![run_json("build", "in_progress", None)]);
    let mut since = HashMap::new();
    rig.pass(&mut since, 1_000).await.unwrap();
    rig.pass(&mut since, 9_000).await.unwrap();
    let sent = rig.store.sent("ci_result");
    assert_eq!(
        (&sent[0]["pendingSince"], &sent[1]["pendingSince"]),
        (&json!(1_000), &json!(1_000))
    );
    // It passes: forgotten, so a later pending head starts again.
    head_is(
        rig.dir.path(),
        "aaa",
        vec![run_json("build", "completed", Some("success"))],
    );
    rig.pass(&mut since, 10_000).await.unwrap();
    assert!(since.is_empty());
    // The head moves on while pending: the old one is forgotten at once.
    list(rig.dir.path(), vec![pull(1, "feat/a", "aab")]);
    head_is(rig.dir.path(), "aab", vec![run_json("build", "in_progress", None)]);
    rig.pass(&mut since, 11_000).await.unwrap();
    list(rig.dir.path(), vec![pull(1, "feat/a", "aac")]);
    head_is(
        rig.dir.path(),
        "aac",
        vec![run_json("build", "completed", Some("success"))],
    );
    rig.pass(&mut since, 12_000).await.unwrap();
    assert!(since.is_empty());
}

#[tokio::test]
async fn a_judge_instruction_runs_the_judge_and_asks_again_with_what_it_did() {
    let rig = Rig::new().await;
    list(rig.dir.path(), vec![pull(4, "feat/x", "ddd")]);
    red(rig.dir.path(), "ddd");
    rig.store.answers(|a| {
        if a["did"].is_null() {
            tell(json!({ "do": "judge", "report": true }))
        } else {
            green()
        }
    });
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let asked = rig.judged();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        (asked[0].0, asked[0].1.as_str(), asked[0].2.as_str()),
        (4, "Title 4", "feat/x")
    );
    assert!(
        asked[0].3.contains("build") && asked[0].3.contains("boom in the build"),
        "{}",
        asked[0].3
    );
    let sent = rig.store.sent("ci_result");
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[1]["did"],
        json!({ "kind": "judge", "verdict": "not-change", "report": true })
    );
    // The second call is the first one's facts with only `did` added.
    let mut again = sent[1].clone();
    again.as_object_mut().unwrap().remove("did");
    assert_eq!(again, sent[0]);
}

#[tokio::test]
async fn a_judge_that_gave_no_verdict_says_so() {
    let mut rig = Rig::new().await;
    rig.judge.says = None;
    list(rig.dir.path(), vec![pull(4, "feat/x", "ddd")]);
    red(rig.dir.path(), "ddd");
    rig.store.answers(|a| {
        if a["did"].is_null() {
            tell(json!({ "do": "judge" }))
        } else {
            green()
        }
    });
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(
        rig.store.sent("ci_result")[1]["did"],
        json!({ "kind": "judge", "verdict": null, "report": false })
    );
}

#[tokio::test]
async fn at_most_four_instructions_run_for_one_head_in_a_pass() {
    let rig = Rig::new().await;
    list(rig.dir.path(), vec![pull(4, "feat/x", "ddd")]);
    red(rig.dir.path(), "ddd");
    rig.store.answers(|_| {
        let mut a = tell(json!({ "do": "judge" }));
        a["logs"] = json!([{ "pr": 4, "what": "again" }]);
        a["bells"] = json!([{ "title": "T", "body": "B" }]);
        a
    });
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    assert_eq!(rig.judged().len(), 4);
    assert_eq!(rig.store.sent("ci_result").len(), 5);
    // The fifth answer's logs and bells are still written and shown, though its instruction is not run.
    assert_eq!(rig.store.sent("log_add").len(), 5);
    assert_eq!(rig.bells.lock().unwrap().len(), 5);
}

#[tokio::test]
async fn logs_go_to_log_add_as_ci_entries_and_bells_are_shown_word_for_word() {
    let rig = Rig::new().await;
    list(rig.dir.path(), vec![pull(4, "feat/x", "ddd")]);
    head_is(
        rig.dir.path(),
        "ddd",
        vec![run_json("build", "completed", Some("success"))],
    );
    rig.store.answers(|_| {
        json!({ "ok": true, "condition": "red", "next": null,
            "logs": [{ "pr": 4, "what": "one", "bell": true }, { "pr": 4, "what": "two" }],
            "bells": [{ "title": "Pull request #4 is red", "body": "Title 4 (at ddd)" }] })
    });
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let logs = rig.store.sent("log_add");
    assert_eq!(
        logs[0],
        json!({ "kind": "ci", "entry": { "pr": 4, "what": "one", "bell": true } })
    );
    assert_eq!(logs[1]["entry"]["what"], "two");
    assert_eq!(
        *rig.bells.lock().unwrap(),
        [("Pull request #4 is red".to_string(), "Title 4 (at ddd)".to_string())]
    );
}

#[tokio::test]
async fn a_pull_request_whose_checks_cant_be_read_is_skipped_not_the_pass() {
    let rig = Rig::new().await;
    list(
        rig.dir.path(),
        vec![pull(1, "fork/a", "nofile"), pull(2, "feat/b", "bbb")],
    );
    head_is(
        rig.dir.path(),
        "bbb",
        vec![run_json("build", "completed", Some("success"))],
    );
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let sent = rig.store.sent("ci_result");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["pr"], 2);
}

#[tokio::test]
async fn fix_is_not_run_and_merge_main_not_where_it_cant_push_and_either_ends_the_carry_out() {
    for next in ["fix", "mergeMain"] {
        let rig = Rig::new().await;
        list(rig.dir.path(), vec![pull(4, "feat/x", "ddd")]);
        red(rig.dir.path(), "ddd");
        rig.store.answers(move |_| {
            let mut a = tell(json!({ "do": next }));
            a["logs"] = json!([{ "pr": 4, "what": "going to try" }]);
            a
        });
        rig.pass(&mut HashMap::new(), 0).await.unwrap();
        assert_eq!(rig.store.sent("ci_result").len(), 1, "{next} was answered");
        assert!(rig.judged().is_empty());
        assert!(rig.run_calls().is_empty(), "{:?}", rig.gh_log());
        assert_eq!(
            rig.store.sent("log_add").len(),
            1,
            "what the store worded is still written"
        );
    }
}

#[tokio::test]
async fn a_retaskable_branch_that_is_a_board_items_is_left_to_its_watch_run() {
    let mut rig = Rig::new().await;
    rig.items = ["retaskable/1-mine".to_string(), "feat/owned".to_string()].into();
    list(
        rig.dir.path(),
        vec![
            pull(1, "retaskable/1-mine", "aaa"),
            pull(2, "retaskable/2-nobody", "bbb"),
            pull(3, "feat/owned", "ccc"),
        ],
    );
    for sha in ["aaa", "bbb", "ccc"] {
        head_is(
            rig.dir.path(),
            sha,
            vec![run_json("build", "completed", Some("success"))],
        );
    }
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let watched: Vec<Value> = rig.store.sent("ci_result").iter().map(|f| f["pr"].clone()).collect();
    assert_eq!(watched, [json!(2), json!(3)]);
    // Its checks were not even read.
    assert!(
        rig.gh_log().iter().all(|l| !l.contains("commits/aaa/")),
        "{:?}",
        rig.gh_log()
    );
}

#[tokio::test]
async fn a_conflicting_head_says_so_and_sends_no_base_head() {
    let rig = Rig::new().await;
    let mut p = pull(1, "feat/a", "aaa");
    p["mergeable"] = json!("CONFLICTING");
    list(rig.dir.path(), vec![p]);
    head_is(
        rig.dir.path(),
        "aaa",
        vec![run_json("build", "completed", Some("success"))],
    );
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let sent = &rig.store.sent("ci_result")[0];
    assert_eq!(sent["conflicting"], true);
    assert!(sent.get("baseHead").is_none());
}

#[tokio::test]
async fn a_store_that_cant_be_reached_ends_the_pass_and_nothing_is_run() {
    let rig = Rig::new().await;
    list(rig.dir.path(), vec![pull(4, "feat/x", "ddd")]);
    red(rig.dir.path(), "ddd");
    rig.store.go_down();
    let err = rig.pass(&mut HashMap::new(), 0).await.unwrap_err();
    assert!(format!("{err:#}").contains("ci_result for pull request #4"), "{err:#}");
    assert!(rig.judged().is_empty());
}

#[tokio::test]
async fn a_list_that_cant_be_read_ends_the_pass() {
    let rig = Rig::new().await;
    std::fs::write(rig.dir.path().join("list.json"), "not json").unwrap();
    assert!(rig.pass(&mut HashMap::new(), 0).await.is_err());
    assert!(rig.store.sent("ci_result").is_empty());
}
