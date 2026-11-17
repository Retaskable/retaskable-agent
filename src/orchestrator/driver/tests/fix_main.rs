//! Fix main from an idle project's baseline (`fix_main.rs`) against a fake store and a real base checkout.
use super::*;
use crate::backlog::runs::RunKind;
use crate::config::CheckConfig;
use crate::test_support::{git, repo_with_origin, run as run_git};
use axum::http::StatusCode;
use std::path::Path;
use std::time::Instant;

#[derive(Default)]
pub(super) struct Script {
    pub(super) log: Vec<Value>,
    pub(super) items: Vec<Value>,
    pub(super) lock: Option<Value>,
    pub(super) list_fails: bool,
    pub(super) lock_fails: bool,
    /// What `chore_trigger` answers (queued when unset), and whether it breaks.
    pub(super) trigger: Option<Value>,
    pub(super) trigger_fails: bool,
    /// What `facts_get` answers, by kind.
    pub(super) facts: Value,
    /// Whether `fact_set` breaks.
    pub(super) fact_set_fails: bool,
}

pub(super) type Store_ = Arc<Mutex<Script>>;

impl Script {
    pub(super) fn args(&self, op: &str) -> Vec<Value> {
        self.log
            .iter()
            .filter(|b| b["op"] == op)
            .map(|b| b["args"].clone())
            .collect()
    }
}

pub(super) async fn store() -> (String, Store_) {
    let script: Store_ = Arc::default();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(
                |axum::extract::State(s): axum::extract::State<Store_>, Json(body): Json<Value>| async move {
                    let mut s = s.lock().unwrap();
                    s.log.push(body.clone());
                    let ok = |v: Value| (StatusCode::OK, Json(json!({ "value": v })));
                    match body["op"].as_str().unwrap() {
                        "list" if s.list_fails => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" }))),
                        "list" => ok(Value::Array(s.items.clone())),
                        "fact_set" if s.fact_set_fails => {
                            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" })))
                        }
                        "fact_set" => ok(json!({})),
                        "facts_get" => ok(if s.facts.is_object() {
                            s.facts.clone()
                        } else {
                            json!({})
                        }),
                        "chore_trigger" if s.trigger_fails => {
                            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" })))
                        }
                        "chore_trigger" => ok(s.trigger.clone().unwrap_or(json!({ "queued": "chore:fix_main" }))),
                        "lock_take" if s.lock_fails => {
                            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" })))
                        }
                        "lock_take" => ok(s.lock.clone().unwrap_or(json!({ "held": true, "token": "L1" }))),
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

pub(super) struct Idle;
impl RunStarter for Idle {
    fn kinds(&self) -> Vec<RunKind> {
        vec![RunKind::Build]
    }
    fn start(&self, _: RunRequest) -> Result<(), String> {
        Err("nothing to start".into())
    }
}

/// An idle project whose base branch is `repo`'s, with one check that exits `code`.
pub(super) struct Fixture {
    pub(super) rig: Rig,
    pub(super) script: Store_,
    pub(super) repo: std::path::PathBuf,
    _root: tempfile::TempDir,
}

pub(super) async fn fixture(code: i32) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let (base, script) = store().await;
    let mut rig = rig(1, &[&base]);
    let lp = &mut rig.machine.projects[0];
    // The baseline caches by project and commit, and these repos' first commits can be the same.
    lp.id = format!("fm-{}", root.path().file_name().unwrap().to_string_lossy());
    lp.mode = OrchestratorMode::Runs;
    lp.takes_tasks = true;
    lp.project.repo_path = repo.display().to_string();
    lp.file_project.repo_path = repo.display().to_string();
    lp.project.worktrees_dir = root.path().join("wt").display().to_string();
    lp.file_project.worktrees_dir = root.path().join("wt").display().to_string();
    lp.project.base_branch = "main".into();
    lp.file_project.base_branch = "main".into();
    lp.project.checks = vec![CheckConfig {
        name: "unit".into(),
        command: format!("echo broke-here && exit {code}"),
        blocking: true,
        timeout_sec: 30,
    }];
    Fixture {
        rig,
        script,
        repo,
        _root: root,
    }
}

/// Passes until the baseline task has come back and been acted on.
async fn settle(f: &mut Fixture) {
    for _ in 0..60 {
        let m = &mut f.rig.machine;
        let shared = Shared {
            links: &m.links,
            local: &m.local,
            claim_paused_until: &mut None,
            claude_signed_out_until: &mut None,
            engine_limits: &mut Default::default(),
            stopping: false,
        };
        m.projects[0]
            .run_pass(&mut m.pool, &Idle, &shared, Instant::now())
            .await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn triggers(f: &Fixture) -> Vec<Value> {
    f.script.lock().unwrap().args("chore_trigger")
}

pub(super) fn push_commit(repo: &Path) -> String {
    run_git(git(repo).args(["commit", "--quiet", "--allow-empty", "-m", "more"]));
    run_git(git(repo).args(["push", "--quiet", "origin", "main"]));
    crate::lanes::head_sha(repo).unwrap()
}

#[tokio::test]
async fn a_red_main_is_reported_once_per_commit_with_the_failing_checks_and_takes_no_lock_and_adds_nothing() {
    let mut f = fixture(1).await;
    settle(&mut f).await;
    let sha = crate::lanes::head_sha(&f.repo).unwrap();
    let sent = triggers(&f);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["trigger"], "red_main");
    let data = &sent[0]["data"];
    assert_eq!(
        (data["sha"].as_str(), data["branch"].as_str()),
        (Some(sha.as_str()), Some("main"))
    );
    assert_eq!(data["checks"], json!([{ "name": "unit" }]));
    let brief = data["brief"].as_str().unwrap();
    assert!(brief.contains("unit") && brief.contains("broke-here"), "{brief}");
    {
        let s = f.script.lock().unwrap();
        assert!(
            s.args("lock_take").is_empty() && s.args("add").is_empty(),
            "{:?}",
            s.log
        );
    }
    // The baseline is kept as the store's fact, with no output.
    let owed = crate::kept_facts::shared(f.rig.machine.projects[0].store.dir()).owed(crate::kept_facts::BASELINE);
    assert_eq!(owed.unwrap()["results"][0]["ok"], false);

    // Main moves and is still red: one more.
    push_commit(&f.repo);
    settle(&mut f).await;
    assert_eq!(triggers(&f).len(), 2);
}

#[tokio::test]
async fn a_green_main_reports_nothing() {
    let mut f = fixture(0).await;
    settle(&mut f).await;
    assert!(triggers(&f).is_empty(), "{:?}", f.script.lock().unwrap().log);
}

#[tokio::test]
async fn a_computer_that_takes_no_tasks_does_not_ask_for_a_fix_it_would_not_run() {
    let mut f = fixture(1).await;
    f.rig.machine.projects[0].takes_tasks = false;
    settle(&mut f).await;
    assert!(
        triggers(&f).is_empty(),
        "a waiting red_main would hold back plans: {:?}",
        f.script.lock().unwrap().log
    );
}

#[tokio::test]
async fn a_fix_the_store_says_found_nothing_to_fix_shows_the_note_until_main_moves() {
    let mut f = fixture(1).await;
    f.script.lock().unwrap().trigger = Some(json!({ "skipped": "a fix found nothing to fix at that commit" }));
    settle(&mut f).await;
    let sha = crate::lanes::head_sha(&f.repo).unwrap();
    assert_eq!(triggers(&f).len(), 1, "told once for the commit");
    let note = f.rig.live.snapshot().projects[&f.rig.machine.projects[0].id]
        .fix_main
        .clone()
        .unwrap();
    assert_eq!(
        note,
        format!(
            "Fixing main found nothing to fix at {}. It tries again when main moves.",
            &sha[..7]
        )
    );
    push_commit(&f.repo);
    settle(&mut f).await;
    assert_eq!(triggers(&f).len(), 2);
}

#[tokio::test]
async fn a_fix_already_open_is_the_stores_skip_and_a_store_that_breaks_is_tried_again_later() {
    let mut f = fixture(1).await;
    f.script.lock().unwrap().trigger = Some(json!({ "skipped": "a fix is already open" }));
    settle(&mut f).await;
    assert_eq!(triggers(&f).len(), 1);
    let id = f.rig.machine.projects[0].id.clone();
    assert_eq!(f.rig.live.snapshot().projects[&id].fix_main, None);

    let mut f = fixture(1).await;
    f.script.lock().unwrap().trigger_fails = true;
    settle(&mut f).await;
    assert_eq!(triggers(&f).len(), 1, "one try, then it waits before another");
    f.script.lock().unwrap().trigger_fails = false;
    f.rig.machine.projects[0].run_loop.fix_main.retry_now();
    settle(&mut f).await;
    assert_eq!(triggers(&f).len(), 2, "told once the store answers");
    f.rig.machine.projects[0].run_loop.fix_main.retry_now();
    settle(&mut f).await;
    assert_eq!(triggers(&f).len(), 2, "and only once for the commit");
}
