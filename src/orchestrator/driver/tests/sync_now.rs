//! Sync now (`sync_now.rs`, `main_sync.rs`): a `sync_requested` row starts the main sync at once, against a fake store that
//! lists rows and items and records every op, real git repos (a bare origin) and a fake `gh`.
use super::fix_main::Idle;
use super::main_sync::{commit, fake_gh, push};
use super::*;
use crate::config::SyncMain;
use crate::test_support::{git, repo_with_origin, run as run_git};
use axum::http::StatusCode;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Script {
    log: Vec<Value>,
    rows: Vec<Value>,
    items: Vec<Value>,
    lock: Option<Value>,
    list_fails: bool,
    lock_fails: bool,
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
                        "list" if s.list_fails => boom(),
                        "list" => ok(Value::Array(s.items.clone())),
                        "lock_take" if s.lock_fails => boom(),
                        "lock_take" => ok(s.lock.clone().unwrap_or(json!({ "held": true, "token": "L1" }))),
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

#[tokio::test]
async fn a_request_starts_the_sync_at_once_past_sync_main_off_and_ends_done_with_the_chore_id() {
    let mut f = fixture().await;
    let sha = crate::lanes::head_sha(&f.repo).unwrap();
    let id = format!("chore-sync-main-{}", &sha[..7]);
    // The minute's gate and `handled` would hold the loop's own look back, as well as `syncMain` being off.
    ask(&f, "in-1", json!({ "dev": true }));
    until(&mut f, "the sync's add and lock drop", |s| {
        !s.args("add").is_empty() && !s.args("lock_drop").is_empty()
    })
    .await;
    let s = f.script.lock().unwrap();
    assert_eq!(
        finishes(&s),
        [json!({ "id": "in-1", "status": "done", "result": { "id": id } })]
    );
    assert_eq!(s.args("lock_take")[0]["name"], "sync_main-team");
    let adds = s.args("add");
    assert_eq!((adds.len(), adds[0]["items"][0]["id"].clone()), (1, json!(id)));
    assert_eq!(adds[0]["items"][0]["kind"], "sync_main");
    assert_eq!(s.args("lock_drop")[0]["token"], "L1");
    drop(s);
    assert!(remote_branches(&f.repo).contains(&format!("retaskable/sync-main-{}", &sha[..7])));
    let log = std::fs::read_to_string(f.root.path().join("gh.log")).unwrap();
    assert_eq!(log.matches("pr create").count(), 1, "{log}");
    assert_eq!(f.rig.machine.projects[0].run_loop.runs().locks().count(), 0);
}

#[tokio::test]
async fn a_paused_project_still_syncs() {
    let mut f = fixture().await;
    f.rig.pause("p0", true);
    ask(&f, "in-1", json!({ "dev": true }));
    until(&mut f, "the sync's add", |s| !s.args("add").is_empty()).await;
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
async fn a_project_on_the_default_branch_is_refused_before_the_store_is_asked_for_items() {
    let mut f = fixture().await;
    f.rig.machine.projects[0].project.base_branch = "main".into();
    let f = refused_with(
        f,
        json!({ "dev": true }),
        "This project works on the repo's default branch, so there's no dev branch to send to it.",
    )
    .await;
    assert!(f.script.lock().unwrap().args("list").is_empty());
}

#[tokio::test]
async fn a_sync_open_in_the_store_is_in_flight() {
    let f = fixture().await;
    f.script.lock().unwrap().items =
        vec![json!({ "id": "chore-sync-main-1", "title": "t", "status": "todo", "rank": 1, "kind": "sync_main" })];
    refused_with(
        f,
        json!({ "dev": true }),
        "A sync of main into team is already in flight.",
    )
    .await;
}

#[tokio::test]
async fn another_computers_lock_is_a_sync_in_flight() {
    let f = fixture().await;
    f.script.lock().unwrap().lock = Some(json!({ "held": false, "by": "other-pc" }));
    let mut f = f;
    ask(&f, "in-1", json!({ "dev": true }));
    let end = ended(&mut f).await;
    assert_eq!(end["status"], "refused");
    assert_eq!(end["why"], "A sync of main into team is already in flight.");
    assert_eq!(remote_branches(&f.repo), "");
    assert_eq!(f.rig.machine.projects[0].run_loop.runs().locks().count(), 0);
}

#[tokio::test]
async fn a_base_with_all_of_main_is_refused() {
    let f = fixture().await;
    run_git(git(&f.repo).args(["push", "--quiet", "--force", "origin", "main:team"]));
    refused_with(f, json!({ "dev": true }), "team already has all of main.").await;
}

#[tokio::test]
async fn a_sync_item_already_made_for_mains_head_is_refused_even_skipped() {
    let f = fixture().await;
    let sha = crate::lanes::head_sha(&f.repo).unwrap();
    f.script.lock().unwrap().items = vec![json!({
        "id": format!("chore-sync-main-{}", &sha[..7]), "title": "t", "status": "skipped", "rank": 1, "kind": "sync_main"
    })];
    let why = "A sync of main into team was already made for main's latest commit.";
    refused_with(f, json!({ "dev": true }), why).await;
}

#[tokio::test]
async fn a_store_that_breaks_fails_the_row_in_plain_words_and_starts_nothing() {
    for lists in [true, false] {
        let mut f = fixture().await;
        {
            let mut s = f.script.lock().unwrap();
            (s.list_fails, s.lock_fails) = (lists, !lists);
        }
        ask(&f, "in-1", json!({ "dev": true }));
        let end = ended(&mut f).await;
        assert_eq!(end["status"], "failed", "{end}");
        let why = end["why"].as_str().unwrap();
        assert!(
            why.starts_with("Couldn't ask the store to sync main into team: "),
            "{why}"
        );
        assert_eq!(remote_branches(&f.repo), "");
        assert!(f.script.lock().unwrap().args("add").is_empty());
        assert_eq!(f.rig.machine.projects[0].run_loop.runs().locks().count(), 0);
    }
}
