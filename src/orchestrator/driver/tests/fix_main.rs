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
struct Fixture {
    rig: Rig,
    script: Store_,
    repo: std::path::PathBuf,
    _root: tempfile::TempDir,
}

async fn fixture(code: i32) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let (base, script) = store().await;
    let mut rig = rig(1, &[&base]);
    let lp = &mut rig.machine.projects[0];
    // The baseline caches by project and commit, and these repos' first commits can be the same.
    lp.id = format!("fm-{}", root.path().file_name().unwrap().to_string_lossy());
    lp.mode = OrchestratorMode::Runs;
    lp.flags = Arc::new(crate::flags::Flags::fixed(&["chore-rotation"]));
    lp.project.repo_path = repo.display().to_string();
    lp.project.worktrees_dir = root.path().join("wt").display().to_string();
    lp.project.base_branch = "main".into();
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

fn adds(f: &Fixture) -> Vec<Value> {
    f.script.lock().unwrap().args("add")
}

fn push_commit(repo: &Path) -> String {
    run_git(git(repo).args(["commit", "--quiet", "--allow-empty", "-m", "more"]));
    run_git(git(repo).args(["push", "--quiet", "origin", "main"]));
    crate::lanes::head_sha(repo).unwrap()
}

#[tokio::test]
async fn a_red_main_takes_the_lock_and_adds_one_fix_main_once_per_commit() {
    let mut f = fixture(1).await;
    settle(&mut f).await;
    let sha = crate::lanes::head_sha(&f.repo).unwrap();
    {
        let s = f.script.lock().unwrap();
        let ops: Vec<_> = s.log.iter().map(|b| b["op"].as_str().unwrap()).collect();
        let take = ops.iter().position(|o| *o == "lock_take").unwrap();
        assert_eq!(
            ops.iter().position(|o| *o == "add").unwrap(),
            take + 2 - 1 + ops[take..].iter().position(|o| *o == "add").unwrap() - 1
        );
        assert_eq!(s.args("lock_take")[0]["name"], "fix_main");
        assert_eq!(s.args("lock_drop")[0]["token"], "L1");
    }
    let adds = adds(&f);
    assert_eq!(adds.len(), 1, "{adds:?}");
    let item = &adds[0]["items"][0];
    assert_eq!(item["redSha"], sha);
    assert_eq!(item["kind"], "fix_main");
    let report = item["remaining"].as_str().unwrap();
    assert!(report.contains("unit") && report.contains("broke-here"), "{report}");
    assert_eq!(f.rig.machine.projects[0].run_loop.runs().locks().count(), 0);

    // Main moves and is still red: one more.
    push_commit(&f.repo);
    settle(&mut f).await;
    assert_eq!(self::adds(&f).len(), 2);
}

#[tokio::test]
async fn a_green_main_sends_no_lock_and_no_add() {
    let mut f = fixture(0).await;
    settle(&mut f).await;
    let s = f.script.lock().unwrap();
    assert!(
        s.args("lock_take").is_empty() && s.args("add").is_empty(),
        "{:?}",
        s.log
    );
}

#[tokio::test]
async fn an_open_fix_main_adds_nothing() {
    let mut f = fixture(1).await;
    f.script.lock().unwrap().items =
        vec![json!({ "id": "chore-1", "title": "t", "status": "todo", "rank": 1, "kind": "fix_main" })];
    settle(&mut f).await;
    assert!(adds(&f).is_empty());
}

#[tokio::test]
async fn no_fix_sha_at_the_head_adds_nothing_and_says_so_until_main_moves() {
    let mut f = fixture(1).await;
    let sha = crate::lanes::head_sha(&f.repo).unwrap();
    f.script.lock().unwrap().items = vec![json!({
        "id": "chore-1", "title": "t", "status": "skipped", "rank": 1, "kind": "fix_main", "noFixSha": sha
    })];
    settle(&mut f).await;
    assert!(adds(&f).is_empty());
    assert!(f.script.lock().unwrap().args("lock_take").is_empty());
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
    assert_eq!(adds(&f).len(), 1);
}

#[tokio::test]
async fn a_held_lock_or_a_store_error_adds_nothing() {
    let mut f = fixture(1).await;
    f.script.lock().unwrap().lock = Some(json!({ "held": false, "by": "other-pc" }));
    settle(&mut f).await;
    assert!(adds(&f).is_empty());
    assert!(!f.script.lock().unwrap().args("lock_take").is_empty());

    let mut f = fixture(1).await;
    f.script.lock().unwrap().list_fails = true;
    settle(&mut f).await;
    assert!(adds(&f).is_empty());
    assert!(f.script.lock().unwrap().args("lock_take").is_empty());
}

#[tokio::test]
async fn with_the_flag_off_nothing_is_checked() {
    let mut f = fixture(1).await;
    f.rig.machine.projects[0].flags = Arc::new(crate::flags::Flags::fixed(&[]));
    settle(&mut f).await;
    assert!(f.script.lock().unwrap().args("list").is_empty());
}

#[tokio::test]
async fn a_fix_that_finds_nothing_after_the_add_shows_the_note_without_a_second_add() {
    let mut f = fixture(1).await;
    settle(&mut f).await;
    assert_eq!(adds(&f).len(), 1);
    let id = f.rig.machine.projects[0].id.clone();
    assert_eq!(f.rig.live.snapshot().projects[&id].fix_main, None);
    let sha = crate::lanes::head_sha(&f.repo).unwrap();
    f.script.lock().unwrap().items = vec![json!({
        "id": "chore-fix-main", "title": "t", "status": "skipped", "rank": 1, "kind": "fix_main", "noFixSha": sha
    })];
    settle(&mut f).await;
    let note = f.rig.live.snapshot().projects[&id].fix_main.clone().unwrap();
    assert!(
        note.contains(&format!("found nothing to fix at {}", &sha[..7])),
        "{note}"
    );
    assert_eq!(adds(&f).len(), 1);
}
