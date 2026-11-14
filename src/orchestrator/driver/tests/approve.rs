//! Approve and Merge into main on a spec draft (`driver/land.rs`): a fake store and a fake host standing in for
//! `git` and `gh`.
use super::*;
use crate::config::ProjectConfig;
use crate::drafts::DrafterResult;
use crate::implementer::publish::Published;
use crate::orchestrator::driver::{DraftHost, DraftJob};
use crate::test_support::{git, repo_with_origin, run};
use crate::view::DraftCommand;
use std::path::{Path, PathBuf};

/// Every call the host was asked for, in order, and what `gh pr merge` and the rebase answer.
#[derive(Default)]
struct Host {
    calls: Mutex<Vec<String>>,
    merges: Mutex<Vec<Result<(), &'static str>>>,
    rebase: Mutex<Option<bool>>,
    /// Answers for the next rebases, before `rebase` is asked.
    rebases: Mutex<Vec<bool>>,
    /// The follow-up of each drafter run.
    runs: Mutex<Vec<String>>,
}

impl Host {
    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

impl DraftHost for Host {
    fn prepare(&self, _: &ProjectConfig, id: &str, _: &str) -> Result<PathBuf> {
        Ok(PathBuf::from(format!("draft-{id}")))
    }
    fn run(&self, job: DraftJob) -> BoxFuture<'static, Result<DrafterResult>> {
        self.calls.lock().unwrap().push(format!("run {}", job.session_id));
        self.runs.lock().unwrap().push(job.follow_up);
        async { Ok(serde_json::from_value(json!({ "status": "pr_opened", "summary": "rebased" })).unwrap()) }.boxed()
    }
    fn publish(&self, _: &ProjectConfig, _: &Path, _: &str, _: &str, _: &str) -> Result<Published> {
        anyhow::bail!("a hand-back must not publish: the PR exists")
    }
    fn remove(&self, _: &ProjectConfig, id: &str) {
        self.calls.lock().unwrap().push(format!("remove {id}"));
    }
    fn merge(&self, _: &ProjectConfig, pr: u64, delete_branch: bool) -> Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("merge {pr} delete={delete_branch}"));
        let mut merges = self.merges.lock().unwrap();
        match if merges.is_empty() { Ok(()) } else { merges.remove(0) } {
            Ok(()) => Ok(()),
            Err(why) => anyhow::bail!(why),
        }
    }
    fn close(&self, _: &ProjectConfig, pr: u64) -> Result<()> {
        self.calls.lock().unwrap().push(format!("close {pr}"));
        Ok(())
    }
    fn rebase(&self, _: &ProjectConfig, id: &str, branch: &str) -> Result<bool> {
        self.calls.lock().unwrap().push(format!("rebase {id} {branch}"));
        let mut queued = self.rebases.lock().unwrap();
        if !queued.is_empty() {
            return Ok(queued.remove(0));
        }
        Ok(self.rebase.lock().unwrap().unwrap_or(true))
    }
    fn push(&self, _: &ProjectConfig, id: &str, branch: &str) -> Result<()> {
        self.calls.lock().unwrap().push(format!("push {id} {branch}"));
        Ok(())
    }
    fn fetch(&self, _: &ProjectConfig) -> Result<()> {
        self.calls.lock().unwrap().push("fetch".into());
        Ok(())
    }
}

/// A store whose `draft_take` hands out a draft waiting for approval with a PR.
async fn store(intake: Value) -> (String, Asked) {
    let asked: Asked = Default::default();
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, intake) = (seen.clone(), intake.clone());
            async move {
                let (op, args) = (body["op"].as_str().unwrap_or("").to_string(), body["args"].clone());
                seen.lock().unwrap().push((op.clone(), args));
                let value = match op.as_str() {
                    "draft_take" => json!({ "token": "lease-d", "draft": { "intake": intake,
                        "id": "d1", "status": "needs_person", "detail": "awaiting_approval", "titles": ["Charts"],
                        "prUrl": "https://github.com/o/r/pull/9", "branch": "retaskable/spec-d1-charts",
                        "itemIds": ["charts-1"], "requestedBy": "ada", "sessionId": "sess-1" } }),
                    "upsert_from_specs" => json!({ "added": ["charts-1"] }),
                    "specs_commit" => Value::Null,
                    "list" => json!([]),
                    _ => Value::Null,
                };
                (axum::http::StatusCode::OK, Json(json!({ "value": value })))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

struct Setup {
    github: Option<Value>,
    paused: bool,
    press: fn() -> DraftCommand,
    merges: Vec<Result<(), &'static str>>,
    rebase: Option<bool>,
    rebases: Vec<bool>,
    intake: Value,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            github: None,
            paused: false,
            press: || DraftCommand::Approve {
                project: "p0".into(),
                draft: "d1".into(),
            },
            merges: Vec::new(),
            rebase: None,
            rebases: Vec::new(),
            intake: json!({}),
        }
    }
}

/// Presses the button on a project whose origin already holds the draft's spec, and passes until the draft's
/// `draft_report` has gone (and, for a merge, the headers).
async fn press(setup: Setup) -> (Arc<Host>, Asked, Vec<Value>, tempfile::TempDir) {
    let (base, asked) = store(setup.intake.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let spec = "# Charts\n\nTheme: Hosting\n\nItems:\n- **charts-1**: Draw them. Depends on: none.\n";
    std::fs::create_dir_all(repo.join("docs/specs")).unwrap();
    std::fs::write(repo.join("docs/specs/charts.md"), spec).unwrap();
    run(git(&repo).args(["add", "-A"]));
    run(git(&repo).args(["commit", "--quiet", "-m", "spec"]));
    run(git(&repo).args(["push", "--quiet", "origin", "main"]));
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.lp(0).project.repo_path = repo.to_string_lossy().into_owned();
    rig.lp(0).file_project.repo_path = repo.to_string_lossy().into_owned();
    rig.lp(0).project.base_branch = "main".into();
    rig.lp(0).file_project.base_branch = "main".into();
    rig.lp(0).project.github = setup.github.clone();
    rig.lp(0).file_project.github = setup.github.clone();
    rig.lp(0).seen.default_branch = Some("main".into());
    rig.pause("p0", setup.paused);
    let host = Arc::new(Host {
        merges: Mutex::new(setup.merges),
        rebase: Mutex::new(setup.rebase),
        rebases: Mutex::new(setup.rebases),
        ..Default::default()
    });
    rig.machine.drafts = Some(host.clone());
    rig.machine.merge_wait = Duration::from_millis(1);
    rig.machine.links.item_commands.push_draft((setup.press)());
    for _ in 0..200 {
        rig.machine.pass().await;
        let sent = reported(&asked);
        let merged = sent.iter().any(|r| r["status"] == "merged");
        if !sent.is_empty() && (!merged || !upserts(&asked).is_empty()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (host, asked.clone(), reported(&asked), root)
}

fn reported(asked: &Asked) -> Vec<Value> {
    (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == "draft_report")
        .map(|(_, a)| a.clone())
        .collect()
}

fn upserts(asked: &Asked) -> Vec<Value> {
    (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == "upsert_from_specs")
        .map(|(_, a)| a.clone())
        .collect()
}

#[tokio::test]
async fn a_clean_merge_removes_the_worktree_and_sends_the_headers_at_once() {
    let (host, asked, sent, _root) = press(Setup::default()).await;
    assert_eq!(host.calls(), ["merge 9 delete=true", "remove d1", "fetch"]);
    assert_eq!(
        (
            sent[0]["id"].as_str(),
            sent[0]["token"].as_str(),
            sent[0]["status"].as_str()
        ),
        (Some("d1"), Some("lease-d"), Some("merged"))
    );
    let up = upserts(&asked);
    assert_eq!(up.len(), 1, "{:?}", ops(&asked));
    assert_eq!(up[0]["approved"], json!(["charts-1"]));
    assert_eq!(up[0]["askedBy"], json!({ "charts-1": "ada" }));
    assert_eq!(up[0]["items"][0]["id"], "charts-1");
}

#[tokio::test]
async fn a_merge_also_happens_while_paused() {
    let (host, asked, sent, _root) = press(Setup {
        paused: true,
        ..Default::default()
    })
    .await;
    assert_eq!(host.calls()[0], "merge 9 delete=true");
    assert_eq!(sent[0]["status"], "merged");
    assert_eq!(upserts(&asked).len(), 1, "sent while paused: {:?}", ops(&asked));
}

#[tokio::test]
async fn a_behind_but_clean_pr_is_rebased_and_merged_after_a_retry() {
    let (host, _asked, sent, _root) = press(Setup {
        merges: vec![Err("head branch is behind"), Err("not mergeable yet")],
        ..Default::default()
    })
    .await;
    assert_eq!(
        host.calls(),
        [
            "merge 9 delete=true",
            "rebase d1 retaskable/spec-d1-charts",
            "merge 9 delete=true",
            "merge 9 delete=true",
            "remove d1",
            "fetch"
        ]
    );
    assert_eq!(sent[0]["status"], "merged");
}

fn person() -> Option<Value> {
    Some(json!({ "land": "person", "protectedBranch": "main" }))
}

#[tokio::test]
async fn the_open_pr_only_pattern_waits_for_merge_into_main() {
    let (host, asked, sent, _root) = press(Setup {
        github: person(),
        ..Default::default()
    })
    .await;
    assert!(host.calls().is_empty(), "Approve merged nothing: {:?}", host.calls());
    assert_eq!(sent[0]["detail"], "awaiting_merge");
    assert!(upserts(&asked).is_empty());
    let (host, asked, sent, _root) = press(Setup {
        github: person(),
        press: || DraftCommand::Merge {
            project: "p0".into(),
            draft: "d1".into(),
        },
        ..Default::default()
    })
    .await;
    assert_eq!(host.calls(), ["merge 9 delete=true", "remove d1", "fetch"]);
    assert_eq!(sent[0]["status"], "merged");
    assert_eq!(upserts(&asked)[0]["approved"], json!(["charts-1"]));
}

#[tokio::test]
async fn a_merge_error_goes_back_to_waiting_with_its_reason() {
    let (host, asked, sent, _root) = press(Setup {
        merges: vec![Err("checks are failing")],
        rebase: Some(false),
        ..Default::default()
    })
    .await;
    assert!(!host.calls().iter().any(|c| c.starts_with("remove")));
    assert_eq!(
        (sent[0]["status"].as_str(), sent[0]["detail"].as_str()),
        (Some("needs_person"), Some("awaiting_approval"))
    );
    assert!(sent[0]["summary"].as_str().unwrap().contains("conflicts with main"));
    assert!(upserts(&asked).is_empty());
    let (_host, _asked, sent, _root) = press(Setup {
        github: person(),
        press: || DraftCommand::Merge {
            project: "p0".into(),
            draft: "d1".into(),
        },
        merges: vec![Err("GraphQL: protected branch")],
        ..Default::default()
    })
    .await;
    assert_eq!(sent[0]["detail"], "awaiting_approval");
    assert!(sent[0]["summary"].as_str().unwrap().contains("protected branch"));
}

#[tokio::test]
async fn discard_takes_the_draft_closes_its_pr_and_reports_discarded() {
    let (host, asked, sent, _root) = press(Setup {
        press: || DraftCommand::Discard {
            project: "p0".into(),
            draft: "d1".into(),
        },
        ..Default::default()
    })
    .await;
    assert_eq!(host.calls(), ["close 9", "remove d1", "fetch"]);
    let ops = ops(&asked);
    let take = ops.iter().position(|o| o == "draft_take").unwrap();
    let report = ops.iter().position(|o| o == "draft_report").unwrap();
    assert!(take < report, "{ops:?}");
    assert_eq!(
        (
            sent[0]["id"].as_str(),
            sent[0]["token"].as_str(),
            sent[0]["status"].as_str()
        ),
        (Some("d1"), Some("lease-d"), Some("discarded"))
    );
    assert!(upserts(&asked).is_empty());
}

#[tokio::test]
async fn a_conflict_resumes_the_drafter_and_merges_with_no_new_approval() {
    let (host, asked, sent, _root) = press(Setup {
        merges: vec![Err("head branch is behind")],
        rebases: vec![false, true],
        ..Default::default()
    })
    .await;
    assert_eq!(
        host.calls(),
        [
            "merge 9 delete=true",
            "rebase d1 retaskable/spec-d1-charts",
            "run sess-1",
            "push d1 retaskable/spec-d1-charts",
            "rebase d1 retaskable/spec-d1-charts",
            "merge 9 delete=true",
            "remove d1",
            "fetch"
        ]
    );
    let runs = host.runs.lock().unwrap().clone();
    assert!(runs[0].starts_with("Rebase the branch onto the base branch") && runs[0].contains("conflicts with main"));
    assert_eq!(sent.len(), 1, "no report of waiting for approval: {sent:?}");
    assert_eq!(sent[0]["status"], "merged");
    assert_eq!(upserts(&asked).len(), 1);
}

#[tokio::test]
async fn a_third_conflict_after_two_hand_backs_waits_for_approval_with_the_reason() {
    let (host, _asked, sent, _root) = press(Setup {
        merges: vec![Err("head branch is behind")],
        rebases: vec![false, false, false],
        ..Default::default()
    })
    .await;
    assert_eq!(host.calls().iter().filter(|c| c.starts_with("run ")).count(), 2);
    assert!(!host.calls().iter().any(|c| c.starts_with("remove")));
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (sent[0]["status"].as_str(), sent[0]["detail"].as_str()),
        (Some("needs_person"), Some("awaiting_approval"))
    );
    let why = sent[0]["summary"].as_str().unwrap();
    assert!(why.contains("conflicts with main") && why.contains("2 times"), "{why}");
}
