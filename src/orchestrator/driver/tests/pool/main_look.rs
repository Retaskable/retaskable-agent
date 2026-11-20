//! The base branch as the store sees it (`driver/main_facts.rs`, `crate::main_look`): `main` in `health` once a fetch has
//! been seen, and the store's `main` job run as a pool task. The fake store checks every `health` as the store does, so a
//! `main` that does not fit fails the test, and each job's end is checked the same way. Nothing here sleeps for a fixed time.
use super::jobs::{job_task, set, until_ended};
use super::*;
use crate::jobs::host::JobHost;
use crate::test_support::{git, repo_with_origin, run as run_git};

/// A host with no tools of its own: the `main` job needs none.
struct NoTools;
impl JobHost for NoTools {}

struct Fixture {
    rig: Rig,
    asked: Asked,
    plan: Arc<Mutex<Plan>>,
    repo: std::path::PathBuf,
    head: String,
    _root: tempfile::TempDir,
}

async fn fixture(tasks: impl FnOnce(&str) -> Vec<Value>) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let head = crate::lanes::head_sha(&repo).unwrap();
    let (mut rig, _fake, asked, plan) = live(tasks(&head)).await;
    rig.machine.jobs = Some(Arc::new(NoTools));
    let (path, wt) = (repo.display().to_string(), root.path().join("wt").display().to_string());
    set(&mut rig, |p| {
        p.repo_path = path.clone();
        p.worktrees_dir = wt.clone();
        p.base_branch = "main".into();
    });
    // The baseline caches by project and commit, and these repos' first commits can be the same.
    let id = format!("pm-{}", root.path().file_name().unwrap().to_string_lossy());
    rig.lp(0).id = id.clone();
    // The one check a person approved on this computer (the gate lays only these over the project's settings).
    rig.machine.links.item_commands = Default::default();
    let dir = rig._dir.path().to_path_buf();
    rig.lp(0).data_dir = Some(dir.clone());
    crate::allowed_commands::AllowedCommands {
        commands_version: 1,
        values: json!({ "checks": [{ "name": "unit", "command": "echo broke-here && exit 1", "blocking": true, "timeoutSec": 30 }] })
            .as_object()
            .unwrap()
            .clone(),
        at: 0,
    }
    .save(&crate::allowed_commands::allowed_commands_file(&dir, &id))
    .unwrap();
    rig.lp(0).seen.default_branch = Some("main".into());
    Fixture {
        rig,
        asked,
        plan,
        repo,
        head,
        _root: root,
    }
}

/// The minute's fetch, as the loop reports it.
async fn fetched(f: &mut Fixture) {
    let m = &mut f.rig.machine;
    m.projects[0].report(&m.links).await;
}

async fn pass(f: &mut Fixture) {
    f.rig.lp(0).pool.every = Some(Duration::ZERO);
    f.rig.machine.pass().await;
}

fn last_health(f: &Fixture) -> Value {
    sent(&f.asked, "health").last().cloned().unwrap()
}

#[tokio::test]
async fn health_says_nothing_of_main_before_a_fetch_and_its_facts_after() {
    let mut f = fixture(|_| vec![]).await;
    pass(&mut f).await;
    assert!(last_health(&f).get("main").is_none(), "{}", last_health(&f));
    fetched(&mut f).await;
    pass(&mut f).await;
    let main = last_health(&f)["main"].clone();
    assert_eq!(
        (main["branch"].as_str(), main["sha"].as_str()),
        (Some("main"), Some(f.head.as_str()))
    );
    assert_eq!(main["tree"], Value::Null, "no workflows here");
    assert!(main.get("busy").is_none() && main.get("behind").is_none() && main.get("baseline").is_none());
}

#[tokio::test]
async fn a_baseline_look_with_no_approved_checks_is_refused() {
    let mut f = fixture(|head| vec![job_task("task-1", "main", json!({ "want": "baseline", "sha": head }))]).await;
    f.rig.lp(0).data_dir = None;
    f.rig.machine.links.item_commands = Default::default();
    set(&mut f.rig, |p| p.checks = vec![]);
    fetched(&mut f).await;
    let ends = until_ended(&mut f.rig, &f.asked, 1).await;
    assert_eq!(ends[0]["ok"], false);
    assert!(
        ends[0]["refused"]["why"].as_str().unwrap().contains("approved"),
        "{}",
        ends[0]
    );
}

#[tokio::test]
async fn busy_says_the_base_checkout_is_in_use_and_a_moved_head_is_the_new_one() {
    let mut f = fixture(|_| vec![]).await;
    fetched(&mut f).await;
    let id = f.rig.lp(0).id.clone();
    let hold = crate::base_hold::hold_base(&id, "Play main").unwrap();
    pass(&mut f).await;
    assert_eq!(last_health(&f)["main"]["busy"], true);
    drop(hold);
    run_git(git(&f.repo).args(["commit", "--quiet", "--allow-empty", "-m", "more"]));
    run_git(git(&f.repo).args(["push", "--quiet", "origin", "main"]));
    let next = crate::lanes::head_sha(&f.repo).unwrap();
    fetched(&mut f).await;
    pass(&mut f).await;
    let main = last_health(&f)["main"].clone();
    assert_eq!(main["sha"], next.as_str());
    assert!(main.get("busy").is_none());
}

#[tokio::test]
async fn a_baseline_job_runs_for_the_head_holds_its_result_for_health_and_ends_ok_with_the_failed_checks_and_a_brief() {
    let mut f = fixture(|head| vec![job_task("task-1", "main", json!({ "want": "baseline", "sha": head }))]).await;
    fetched(&mut f).await;
    let ends = until_ended(&mut f.rig, &f.asked, 1).await;
    assert_eq!(ends[0]["id"], "task-1");
    assert_eq!(ends[0]["ok"], true, "{}", ends[0]);
    let result = &ends[0]["result"];
    crate::backlog::pool::contract::main_result(result).unwrap();
    assert_eq!(result["baseline"], json!({ "sha": f.head, "failed": ["unit"] }));
    assert!(result["brief"].as_str().unwrap().contains("broke-here"), "{result}");
    // The next health carries what it holds, so the store sees the result both ways.
    pass(&mut f).await;
    assert_eq!(
        last_health(&f)["main"]["baseline"],
        json!({ "sha": f.head, "failed": ["unit"] })
    );
    // The store wants nothing asked of it: a job ends once.
    assert_eq!(sent(&f.asked, "task_end").len(), 1);
}

#[tokio::test]
async fn main_jobs_run_one_at_a_time_take_no_slot_and_run_while_the_store_holds_work_back() {
    let mut f = fixture(|head| {
        vec![
            job_task("task-1", "main", json!({ "want": "coverage", "sha": head })),
            job_task("task-2", "main", json!({ "want": "baseline", "sha": head })),
        ]
    })
    .await;
    f.plan.lock().unwrap().wait = Some("paused");
    fetched(&mut f).await;
    let ends = until_ended(&mut f.rig, &f.asked, 2).await;
    let order: Vec<&str> = ends.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(order, ["task-1", "task-2"], "serialised, in the order sent");
    assert!(ends.iter().all(|e| e["ok"] == true), "{ends:?}");
    assert!(sent(&f.asked, "claim").is_empty());
}

#[tokio::test]
async fn a_main_job_for_a_head_that_moved_on_ends_ok_with_the_bare_sha_and_a_ci_look_is_refused_off_github() {
    let mut f = fixture(|_| {
        vec![
            job_task("task-1", "main", json!({ "want": "ci", "sha": "1".repeat(40) })),
            job_task("task-2", "main", json!({ "want": "ci", "sha": "2".repeat(40) })),
        ]
    })
    .await;
    fetched(&mut f).await;
    let ends = until_ended(&mut f.rig, &f.asked, 2).await;
    assert_eq!(ends[0]["result"], json!({ "sha": "1".repeat(40) }));
    assert_eq!(ends[0]["ok"], true);
    assert_eq!(ends[1]["result"], json!({ "sha": "2".repeat(40) }));
}

#[tokio::test]
async fn a_ci_look_on_a_commit_that_is_the_head_is_refused_when_the_host_cannot_say() {
    let mut f = fixture(|head| vec![job_task("task-1", "main", json!({ "want": "ci", "sha": head }))]).await;
    set(&mut f.rig, |p| {
        p.code_host = serde_json::from_value(json!({ "kind": "gitlab" })).unwrap()
    });
    fetched(&mut f).await;
    let ends = until_ended(&mut f.rig, &f.asked, 1).await;
    assert_eq!(ends[0]["ok"], false);
    assert_eq!(ends[0]["refused"]["why"], "The code host can't say what CI found.");
}

fn commit(repo: &std::path::Path, name: &str) {
    std::fs::write(repo.join(name), name).unwrap();
    run_git(git(repo).args(["add", name]));
    run_git(git(repo).args(["commit", "--quiet", "-m", name]));
}

fn origin_sha(repo: &std::path::Path, branch: &str) -> String {
    let out = git(repo)
        .args(["ls-remote", "origin", &format!("refs/heads/{branch}")])
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

/// An integration-branch project: its base `dev` has a commit of its own and the default branch `main` is one ahead.
/// `task` makes the tasks the store sends, from the heads of `dev` and `main`.
async fn integration(task: impl FnOnce(&str, &str) -> Vec<Value>) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    run_git(git(&repo).args(["checkout", "--quiet", "-b", "dev"]));
    commit(&repo, "d.txt");
    run_git(git(&repo).args(["push", "--quiet", "origin", "dev"]));
    run_git(git(&repo).args(["checkout", "--quiet", "main"]));
    commit(&repo, "m.txt");
    run_git(git(&repo).args(["push", "--quiet", "origin", "main"]));
    let (dev, main) = (origin_sha(&repo, "dev"), origin_sha(&repo, "main"));
    let (mut rig, _fake, asked, plan) = live(task(&dev, &main)).await;
    rig.machine.jobs = Some(Arc::new(NoTools));
    let (path, wt) = (repo.display().to_string(), root.path().join("wt").display().to_string());
    set(&mut rig, |p| {
        p.repo_path = path.clone();
        p.worktrees_dir = wt.clone();
        p.base_branch = "dev".into();
        p.github = Some(json!({ "pattern": "integration" }));
    });
    rig.lp(0).id = format!("pi-{}", root.path().file_name().unwrap().to_string_lossy());
    rig.lp(0).seen.default_branch = Some("main".into());
    Fixture {
        rig,
        asked,
        plan,
        repo,
        head: dev,
        _root: root,
    }
}

#[tokio::test]
async fn an_integration_branch_reports_a_rebase_fact_in_place_of_behind_and_what_waits_is_read_live() {
    let mut f = integration(|_, _| vec![]).await;
    fetched(&mut f).await;
    pass(&mut f).await;
    let main = origin_sha(&f.repo, "main");
    let rebase = last_health(&f)["main"]["rebase"].clone();
    assert_eq!(
        rebase,
        json!({ "branch": "main", "head": main, "base": f.head, "count": 1, "waiting": [] })
    );
    assert!(last_health(&f)["main"].get("behind").is_none());
    // An item cut from the branch is on its way in: the next report names it, with no fetch between.
    let mut run = crate::item_lifecycle::ItemRun::new();
    run.state = crate::item_lifecycle::ItemState::Implementing;
    f.rig.lp(0).runs.insert("a-1".into(), run);
    pass(&mut f).await;
    assert_eq!(last_health(&f)["main"]["rebase"]["waiting"], json!(["a-1"]));
    // A project that syncs by a pull request reports the old fact instead.
    set(&mut f.rig, |p| p.github = None);
    fetched(&mut f).await;
    pass(&mut f).await;
    let main_facts = last_health(&f)["main"].clone();
    assert_eq!(main_facts["behind"]["count"], 1);
    assert!(main_facts.get("rebase").is_none());
}

#[tokio::test]
async fn a_rebase_job_rebases_the_base_branch_and_ends_ok_with_an_empty_rebase() {
    let mut f = integration(|dev, main| {
        vec![job_task(
            "task-1",
            "main",
            json!({ "want": "rebase", "sha": dev, "main": "main", "mainSha": main }),
        )]
    })
    .await;
    fetched(&mut f).await;
    let ends = until_ended(&mut f.rig, &f.asked, 1).await;
    assert_eq!(ends[0]["ok"], true, "{}", ends[0]);
    assert_eq!(ends[0]["result"], json!({ "sha": f.head, "rebase": {} }));
    crate::backlog::pool::contract::main_result(&ends[0]["result"]).unwrap();
    assert_ne!(origin_sha(&f.repo, "dev"), f.head, "pushed");
    // No timer of its own: it rebases only when told, and the agent sends the store no trigger about it.
    for _ in 0..3 {
        pass(&mut f).await;
    }
    assert_eq!(sent(&f.asked, "task_end").len(), 1);
    assert!(sent(&f.asked, "chore_trigger").is_empty());
}

#[tokio::test]
async fn a_rebase_job_answers_busy_while_something_waits_and_does_not_touch_the_branch() {
    let mut f = integration(|dev, main| {
        vec![job_task(
            "task-1",
            "main",
            json!({ "want": "rebase", "sha": dev, "main": "main", "mainSha": main }),
        )]
    })
    .await;
    let mut run = crate::item_lifecycle::ItemRun::new();
    run.state = crate::item_lifecycle::ItemState::Implementing;
    f.rig.lp(0).runs.insert("a-1".into(), run);
    fetched(&mut f).await;
    let ends = until_ended(&mut f.rig, &f.asked, 1).await;
    assert_eq!(ends[0]["result"], json!({ "sha": f.head, "busy": true }));
    assert_eq!(origin_sha(&f.repo, "dev"), f.head);
}
