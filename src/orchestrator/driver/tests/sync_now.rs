//! Sync now (`sync_now.rs`, `main_sync.rs`): a `sync_requested` row starts the main sync at once, against a fake store that
//! lists rows and items and records every op, real git repos (a bare origin) and a fake `gh`.
use super::main_sync::{commit, fake_gh, push};
use super::store_script::Idle;
use super::*;
use crate::config::SyncMain;
use crate::test_support::{git, repo_with_origin, run as run_git};
use axum::http::StatusCode;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Script {
    log: Vec<Value>,
    rows: Vec<Value>,
    /// What `chore_trigger` answers (a task `task-1` when unset), and whether it breaks.
    trigger: Option<Value>,
    trigger_fails: bool,
}

type Store_ = Arc<Mutex<Script>>;

impl Script {
    fn args(&self, op: &str) -> Vec<Value> {
        (self.log.iter())
            .filter(|b| b["op"] == op)
            .map(|b| b["args"].clone())
            .collect()
    }
}

async fn store() -> (String, Store_) {
    let script: Store_ = Arc::default();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(
                |axum::extract::State(s): axum::extract::State<Store_>, Json(body): Json<Value>| async move {
                    let mut s = s.lock().unwrap();
                    s.log.push(body.clone());
                    let ok = |v: Value| (StatusCode::OK, Json(json!({ "value": v })));
                    let boom = || (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" })));
                    match body["op"].as_str().unwrap() {
                        "chore_trigger" if s.trigger_fails => boom(),
                        "chore_trigger" => ok(s.trigger.clone().unwrap_or(json!({ "task": "task-1" }))),
                        "pause_get" => ok(json!({ "paused": null, "log": [] })),
                        "intent_list" => ok(Value::Array(s.rows.clone())),
                        "intent_claim" => ok(json!({ "won": true })),
                        "intent_beat" => ok(json!({ "ok": true })),
                        "intent_finish" => {
                            let (id, status) = (body["args"]["id"].clone(), body["args"]["status"].clone());
                            (s.rows.iter_mut().filter(|r| r["id"] == id)).for_each(|r| r["status"] = status.clone());
                            ok(json!({ "ok": true }))
                        }
                        _ => ok(Value::Null),
                    }
                },
            ),
        )
        .with_state(script.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), script)
}

struct Fixture {
    rig: Rig,
    script: Store_,
    repo: PathBuf,
    root: tempfile::TempDir,
}

/// origin has `main` one commit ahead of `team` (which has its own commit); the project's base is `team`, in `runs`
/// mode with `syncMain` off, so only a person's request can start a sync.
async fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    run_git(git(&repo).args(["checkout", "--quiet", "-b", "team"]));
    commit(&repo, "c.txt", "team side");
    push(&repo, "team");
    run_git(git(&repo).args(["checkout", "--quiet", "main"]));
    commit(&repo, "m.txt", "main side");
    push(&repo, "main");
    let (base, script) = store().await;
    let mut rig = rig(1, &[&base]);
    rig.machine.run_starters = crate::orchestrator::driver::run_starters::RunStarters(vec![Arc::new(Idle)]);
    // A computer that takes tasks (it has a session runner) beside its run runner: the hybrid.
    let _fake = rig.go_live();
    let lp = &mut rig.machine.projects[0];
    lp.mode = OrchestratorMode::Runs;
    for project in [&mut lp.project, &mut lp.file_project] {
        project.repo_path = repo.display().to_string();
        project.worktrees_dir = root.path().join("wt").display().to_string();
        project.base_branch = "team".into();
        project.sync_main = SyncMain::Off;
    }
    lp.seen.default_branch = Some("main".into());
    lp.syncing.poll_every = Duration::ZERO;
    lp.run_loop.main_sync = crate::orchestrator::driver::main_sync::State::with_gh(fake_gh(root.path()));
    Fixture {
        rig,
        script,
        repo,
        root,
    }
}

fn row(id: &str, args: Value) -> Value {
    json!({ "id": id, "kind": "sync_requested", "status": "requested", "issuedBy": "ana", "args": args })
}

fn ask(f: &Fixture, id: &str, args: Value) {
    f.script.lock().unwrap().rows.push(row(id, args));
}

/// Passes until `done` holds of the store's log, a bounded number of times.
async fn until(f: &mut Fixture, what: &str, done: impl Fn(&Script) -> bool) {
    for _ in 0..300 {
        f.rig.machine.run_passes().await;
        if done(&f.script.lock().unwrap()) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("never saw {what}: {:?}", f.script.lock().unwrap().log);
}

fn finishes(s: &Script) -> Vec<Value> {
    s.args("intent_finish")
}

/// The one finish a refused or failed request ends with.
async fn ended(f: &mut Fixture) -> Value {
    until(f, "the row's finish", |s| !finishes(s).is_empty()).await;
    finishes(&f.script.lock().unwrap())[0].clone()
}

async fn refused_with(mut f: Fixture, args: Value, why: &str) -> Fixture {
    ask(&f, "in-1", args);
    let end = ended(&mut f).await;
    assert_eq!(end["status"], "refused", "{end}");
    assert_eq!(end["why"], why);
    let s = f.script.lock().unwrap();
    assert!(s.args("lock_take").is_empty() && s.args("add").is_empty());
    drop(s);
    f
}

fn remote_branches(repo: &Path) -> String {
    let out = git(repo)
        .args(["ls-remote", "origin", "refs/heads/retaskable/*"])
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap()
}

fn triggers(f: &Fixture) -> Vec<Value> {
    f.script.lock().unwrap().args("chore_trigger")
}

#[tokio::test]
async fn a_request_asks_the_store_for_the_sync_at_once_past_sync_main_off_and_ends_done_with_the_tasks_id() {
    let mut f = fixture().await;
    ask(&f, "in-1", json!({ "dev": true }));
    until(&mut f, "the row's finish", |s| !finishes(s).is_empty()).await;
    let s = f.script.lock().unwrap();
    assert_eq!(
        finishes(&s),
        [json!({ "id": "in-1", "status": "done", "result": { "id": "task-1" } })]
    );
    assert!(
        s.args("lock_take").is_empty() && s.args("add").is_empty(),
        "{:?}",
        s.log
    );
    drop(s);
    assert_eq!(
        triggers(&f),
        [
            json!({ "trigger": "behind_main", "branch": "team", "main": "main", "behind": 1, "project": f.rig.machine.projects[0].id })
        ]
    );
    // The merge is the task's, run when `health` sends it back (`chores.rs`): nothing was pushed here.
    assert_eq!(remote_branches(&f.repo), "");
}

#[tokio::test]
async fn a_paused_project_still_asks() {
    let mut f = fixture().await;
    f.rig.pause("p0", true);
    ask(&f, "in-1", json!({ "dev": true }));
    until(&mut f, "the row's finish", |s| !finishes(s).is_empty()).await;
    assert_eq!(finishes(&f.script.lock().unwrap())[0]["status"], "done");
}

#[tokio::test]
async fn a_request_without_dev_is_refused_and_nothing_starts() {
    for args in [json!({}), json!({ "dev": false })] {
        let f = fixture().await;
        let f = refused_with(f, args, "This project's code isn't synced anywhere else.").await;
        assert_eq!(remote_branches(&f.repo), "");
    }
}

#[tokio::test]
async fn a_project_on_the_default_branch_is_refused_before_the_store_is_asked() {
    let mut f = fixture().await;
    f.rig.machine.projects[0].project.base_branch = "main".into();
    let f = refused_with(
        f,
        json!({ "dev": true }),
        "This project works on the repo's default branch, so there's no dev branch to send to it.",
    )
    .await;
    assert!(triggers(&f).is_empty());
}

#[tokio::test]
async fn a_sync_the_store_says_is_open_or_a_paused_project_is_refused_in_the_stores_words() {
    let f = fixture().await;
    f.script.lock().unwrap().trigger = Some(json!({ "skipped": "open" }));
    refused_with(
        f,
        json!({ "dev": true }),
        "A sync of main into team is already in flight.",
    )
    .await;
    let f = fixture().await;
    f.script.lock().unwrap().trigger = Some(json!({ "skipped": "paused" }));
    refused_with(
        f,
        json!({ "dev": true }),
        "The project is paused, so nothing was started.",
    )
    .await;
}

#[tokio::test]
async fn a_sync_open_here_is_in_flight_and_the_store_is_not_asked() {
    let mut f = fixture().await;
    let task: crate::backlog::pool::TaskInput =
        serde_json::from_value(json!({ "id": "t", "subject": "chore:sync_main:team", "prompt": "" })).unwrap();
    f.rig.machine.projects[0].chores.insert(
        "t".into(),
        crate::chore_items::sync_work(&task, "abc", "main", "team", &[]),
    );
    let f = refused_with(
        f,
        json!({ "dev": true }),
        "A sync of main into team is already in flight.",
    )
    .await;
    assert!(triggers(&f).is_empty());
}

#[tokio::test]
async fn a_computer_that_takes_no_tasks_cannot_run_the_sync_so_does_not_ask() {
    let mut f = fixture().await;
    f.rig.machine.starter = None;
    let f = refused_with(
        f,
        json!({ "dev": true }),
        "This computer takes no tasks from the store, so it can't run the sync.",
    )
    .await;
    assert!(triggers(&f).is_empty());
}

#[tokio::test]
async fn a_base_with_all_of_main_is_refused() {
    let f = fixture().await;
    run_git(git(&f.repo).args(["push", "--quiet", "--force", "origin", "main:team"]));
    refused_with(f, json!({ "dev": true }), "team already has all of main.").await;
}

#[tokio::test]
async fn a_store_that_breaks_fails_the_row_in_plain_words_and_starts_nothing() {
    let mut f = fixture().await;
    f.script.lock().unwrap().trigger_fails = true;
    ask(&f, "in-1", json!({ "dev": true }));
    let end = ended(&mut f).await;
    assert_eq!(end["status"], "failed", "{end}");
    let why = end["why"].as_str().unwrap();
    assert!(
        why.starts_with("Couldn't ask the store to sync main into team: "),
        "{why}"
    );
    assert_eq!(remote_branches(&f.repo), "");
}

fn integration(f: &mut Fixture) {
    let lp = &mut f.rig.machine.projects[0];
    // The base checkout is held by project id, process-wide: a rebase here must not meet another test's hold.
    lp.id = format!("sn-{}", f.root.path().file_name().unwrap().to_string_lossy());
    for project in [&mut lp.project, &mut lp.file_project] {
        project.github = Some(json!({ "pattern": "integration" }));
    }
}

fn origin_sha(repo: &Path, branch: &str) -> String {
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

#[tokio::test]
async fn with_the_integration_pattern_sync_now_rebases_at_once_and_asks_the_store_for_nothing() {
    let mut f = fixture().await;
    integration(&mut f);
    let (team, main) = (origin_sha(&f.repo, "team"), origin_sha(&f.repo, "main"));
    ask(&f, "in-1", json!({ "dev": true }));
    until(&mut f, "the row's finish", |s| !finishes(s).is_empty()).await;
    assert_eq!(finishes(&f.script.lock().unwrap())[0]["status"], "done");
    assert!(triggers(&f).is_empty(), "a person's ask is run here, not made a task");
    let new = origin_sha(&f.repo, "team");
    assert_ne!(new, team, "the base branch was rebased");
    run_git(git(&f.repo).args(["fetch", "--quiet", "origin"]));
    let out = git(&f.repo)
        .args(["merge-base", "--is-ancestor", &main, &new])
        .output()
        .unwrap();
    assert!(out.status.success(), "it now holds all of main");
    assert_eq!(origin_sha(&f.repo, "main"), main);
}

#[tokio::test]
async fn a_rebase_waits_for_what_is_still_to_go_into_the_branch_and_says_what() {
    let mut f = fixture().await;
    integration(&mut f);
    let team = origin_sha(&f.repo, "team");
    let mut run = crate::item_lifecycle::ItemRun::new();
    run.state = crate::item_lifecycle::ItemState::Implementing;
    f.rig.machine.projects[0].runs.insert("a-1".into(), run);
    let f = refused_with(f, json!({ "dev": true }), "Waiting for a-1 to go into team first.").await;
    assert_eq!(origin_sha(&f.repo, "team"), team, "nothing was rebased");
}

#[tokio::test]
async fn a_conflicting_rebase_names_the_files_and_pushes_nothing() {
    let mut f = fixture().await;
    integration(&mut f);
    commit(&f.repo, "c.txt", "main's own c");
    push(&f.repo, "main");
    let team = origin_sha(&f.repo, "team");
    let f = refused_with(
        f,
        json!({ "dev": true }),
        "team needs a rebase onto main: it conflicts in c.txt.",
    )
    .await;
    assert_eq!(origin_sha(&f.repo, "team"), team);
}
