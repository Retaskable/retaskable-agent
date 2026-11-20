//! What the fetch saw, told to the store (`driver/report.rs`): the Items headers and the landed items.
use super::*;
use crate::config::DeployTarget;
use crate::test_support::{git, repo_with_origin, run};
use std::path::{Path, PathBuf};

/// A store that answers `specs_commit` with nothing, `list` with no items, `upsert_from_specs` with every item
/// sent as new, `awaiting_landing` with `awaiting`, and `landed` by moving every item sent; and serves `served`
/// as production's `/version.json`.
async fn store(awaiting: Arc<Mutex<Value>>, served: Arc<Mutex<String>>) -> (String, Asked) {
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(move |Json(body): Json<Value>| {
                let (seen, awaiting) = (seen.clone(), awaiting.clone());
                async move {
                    let (op, args) = (body["op"].as_str().unwrap_or("").to_string(), body["args"].clone());
                    seen.lock().unwrap().push((op.clone(), args.clone()));
                    let value = match op.as_str() {
                        "specs_commit" => Value::Null,
                        "facts_get" => json!({}),
                        "upsert_from_specs" => json!({ "added": args["items"].as_array().unwrap().iter().map(|i| i["id"].clone()).collect::<Vec<_>>() }),
                        "awaiting_landing" => awaiting.lock().unwrap().clone(),
                        "landed" => args["items"].clone(),
                        _ => json!([]),
                    };
                    Json(json!({ "value": value }))
                }
            }),
        )
        .route(
            "/version.json",
            axum::routing::get(move || {
                let served = served.clone();
                async move { Json(json!({ "sha": served.lock().unwrap().clone() })) }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

/// Commits `file` with `text` and pushes it to origin's `main`; the new head.
fn push(repo: &Path, file: &str, text: &str) -> String {
    let path = repo.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    run(git(repo).args(["add", "-A"]));
    run(git(repo).args(["commit", "--quiet", "-m", file]));
    run(git(repo).args(["push", "--quiet", "origin", "main"]));
    let out = git(repo).args(["rev-parse", "HEAD"]).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A live project on a real repo with origin, its default branch known.
fn live_rig(base: &str, root: &Path) -> (Rig, PathBuf) {
    let repo = repo_with_origin(root);
    let mut rig = rig(2, &[base]);
    rig.go_live();
    rig.lp(0).project.repo_path = repo.to_string_lossy().into_owned();
    rig.lp(0).file_project.repo_path = repo.to_string_lossy().into_owned();
    rig.lp(0).seen.default_branch = Some("main".into());
    (rig, repo)
}

fn landed(asked: &Asked) -> Vec<(String, Value)> {
    let asked = asked.lock().unwrap();
    let sent = asked.iter().filter(|(op, _)| op == "landed");
    sent.map(|(_, args)| (args["where"].as_str().unwrap().to_string(), args["items"].clone()))
        .collect()
}

#[tokio::test]
async fn the_items_headers_go_to_the_store_once_per_change_of_the_specs() {
    let (base, asked) = store(Arc::new(Mutex::new(json!([]))), Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let (mut rig, repo) = live_rig(&base, root.path());
    let header = "# Cloud\n\nTheme: Hosting\n\nItems:\n- **cloud-link**: Link it. Depends on: none.\n\
        - **cloud-sync**: Sync it. Depends on: cloud-link, nowhere.\n\n## 1. Body\n";
    let commit = push(&repo, "docs/specs/cloud.md", header);
    rig.pause("p0", true);
    rig.machine.fetch().await;
    let upserts: Vec<Value> = (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == "upsert_from_specs")
        .map(|(_, args)| args.clone())
        .collect();
    assert_eq!(upserts.len(), 1, "sent while paused too: {:?}", ops(&asked));
    assert_eq!(
        upserts[0],
        json!({
            "commit": commit,
            "previous": null,
            "items": [
                { "id": "cloud-link", "title": "Link it", "dependsOn": [], "specRefs": ["docs/specs/cloud.md"], "milestone": "Hosting" },
                { "id": "cloud-sync", "title": "Sync it", "dependsOn": ["cloud-link"], "specRefs": ["docs/specs/cloud.md"], "milestone": "Hosting" }
            ],
            "approved": [],
            "askedBy": {}
        }),
        "a dependency nothing names is left off"
    );
    asked.lock().unwrap().clear();
    rig.machine.fetch().await;
    assert!(
        ops(&asked).is_empty(),
        "nothing moved, nothing is asked: {:?}",
        ops(&asked)
    );
    push(&repo, "README.md", "a change outside the specs");
    rig.machine.fetch().await;
    assert!(
        !ops(&asked).contains(&"upsert_from_specs".to_string()),
        "the specs didn't change"
    );
}

#[tokio::test]
async fn a_cycle_holds_back_every_header() {
    let (base, asked) = store(Arc::new(Mutex::new(json!([]))), Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let (mut rig, repo) = live_rig(&base, root.path());
    let header = "Items:\n- **a**: A. Depends on: b.\n- **b**: B. Depends on: a.\n- **c**: C. Depends on: none.\n";
    push(&repo, "docs/specs/loop.md", header);
    rig.machine.fetch().await;
    assert!(!ops(&asked).contains(&"upsert_from_specs".to_string()));
}

#[tokio::test]
async fn merged_items_land_on_main_and_in_production_with_no_production_of_its_own() {
    let awaiting = Arc::new(Mutex::new(json!([])));
    let (base, asked) = store(awaiting.clone(), Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let (mut rig, repo) = live_rig(&base, root.path());
    let merged = push(&repo, "src.txt", "merged");
    *awaiting.lock().unwrap() = json!([
        { "id": "x", "status": "on_branch", "mergeCommit": merged },
        { "id": "y", "status": "on_branch", "mergeCommit": "0123456789012345678901234567890123456789" },
        { "id": "z", "status": "on_main", "mergeCommit": merged }
    ]);
    rig.machine.fetch().await;
    assert_eq!(
        landed(&asked),
        [
            ("main".to_string(), json!(["x"])),
            ("production".to_string(), json!(["x", "z"]))
        ],
        "a commit this clone doesn't know hasn't landed"
    );
    // The roll-up names its branch so the store smokes it only when staging deploys from there; production names none.
    let bases: Vec<(String, Option<String>)> = (asked.lock().unwrap().iter())
        .filter(|(op, _)| op == "landed")
        .map(|(_, a)| {
            (
                a["where"].as_str().unwrap().into(),
                a["base"].as_str().map(String::from),
            )
        })
        .collect();
    assert_eq!(
        bases,
        [
            ("main".to_string(), Some("main".to_string())),
            ("production".to_string(), None)
        ]
    );
    asked.lock().unwrap().clear();
    rig.machine.fetch().await;
    assert!(landed(&asked).is_empty(), "main didn't move");
}

#[tokio::test]
async fn production_lands_what_its_branch_or_its_deploy_serves() {
    let awaiting = Arc::new(Mutex::new(json!([])));
    let served = Arc::new(Mutex::new(String::new()));
    let (base, asked) = store(awaiting.clone(), served.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let (mut rig, repo) = live_rig(&base, root.path());
    let merged = push(&repo, "src.txt", "merged");
    *awaiting.lock().unwrap() = json!([{ "id": "z", "status": "on_main", "mergeCommit": merged }]);
    let production = |url: Option<String>| DeployTarget {
        rollout: None,
        branch: "prod".into(),
        url,
        smoke: None,
        version_path: None,
    };
    rig.lp(0).project.deploy.production = Some(production(None));
    rig.lp(0).file_project.deploy.production = Some(production(None));
    rig.machine.fetch().await;
    assert!(landed(&asked).is_empty(), "no production branch yet");
    run(git(&repo).args(["push", "--quiet", "origin", "main:prod"]));
    rig.machine.fetch().await;
    assert_eq!(landed(&asked), [("production".to_string(), json!(["z"]))]);

    // With a URL, only what production says it serves counts.
    asked.lock().unwrap().clear();
    rig.lp(0).project.deploy.production = Some(production(Some(format!("{base}/"))));
    rig.lp(0).file_project.deploy.production = Some(production(Some(format!("{base}/"))));
    rig.lp(0).seen = Default::default();
    rig.lp(0).seen.default_branch = Some("main".into());
    rig.machine.fetch().await;
    assert!(landed(&asked).is_empty(), "production serves nothing yet");
    *served.lock().unwrap() = merged[..7].to_string();
    rig.machine.fetch().await;
    assert_eq!(landed(&asked), [("production".to_string(), json!(["z"]))]);
}

#[tokio::test]
async fn production_is_not_looked_at_until_the_store_has_sent_how_long_a_deploy_is_given() {
    let awaiting = Arc::new(Mutex::new(json!([])));
    let served = Arc::new(Mutex::new(String::new()));
    let (base, asked) = store(awaiting.clone(), served.clone()).await;
    let root = tempfile::tempdir().unwrap();
    let (mut rig, repo) = live_rig(&base, root.path());
    let merged = push(&repo, "src.txt", "merged");
    *awaiting.lock().unwrap() = json!([{ "id": "z", "status": "on_main", "mergeCommit": merged }]);
    let url = Some(format!("{base}/"));
    let production = DeployTarget {
        rollout: None,
        branch: "prod".into(),
        url,
        smoke: None,
        version_path: None,
    };
    rig.lp(0).project.deploy.production = Some(production.clone());
    rig.lp(0).file_project.deploy.production = Some(production);
    run(git(&repo).args(["push", "--quiet", "origin", "main:prod"]));
    *served.lock().unwrap() = merged[..7].to_string();
    rig.lp(0).take_store_tuning(None);
    rig.machine.fetch().await;
    assert!(landed(&asked).is_empty(), "no numbers: production is not looked at");
    rig.lp(0)
        .take_store_tuning(Some(crate::store_tuning::StoreTuning::test()));
    rig.lp(0).seen = Default::default();
    rig.lp(0).seen.default_branch = Some("main".into());
    rig.machine.fetch().await;
    assert_eq!(landed(&asked), [("production".to_string(), json!(["z"]))]);
}

#[tokio::test]
async fn a_held_base_puts_the_sync_off_until_it_is_given_back() {
    let (base, asked) = store(Arc::new(Mutex::new(json!([]))), Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let (mut rig, repo) = live_rig(&base, root.path());
    let id = format!("{}p0", rig.machine.base_scope);
    push(
        &repo,
        "docs/specs/cloud.md",
        "Items:\n- **cloud-link**: Link it. Depends on: none.\n",
    );
    let shoot = crate::base_hold::hold_base(&id, "a shoot").unwrap();
    rig.machine.fetch().await;
    assert!(ops(&asked).is_empty(), "put off, not fetched: {:?}", ops(&asked));
    assert!(!kinds(&mut rig).contains(&"error".to_string()));
    rig.machine.fetch_due().await;
    assert!(ops(&asked).is_empty(), "not due while held");
    drop(shoot);
    rig.machine.fetch_due().await;
    assert!(
        ops(&asked).contains(&"upsert_from_specs".to_string()),
        "{:?}",
        ops(&asked)
    );
    assert_eq!(crate::base_hold::base_holder(&id), None);
}

#[tokio::test]
async fn the_sync_gives_the_base_back_after_a_failed_fetch() {
    let (base, _asked) = store(Arc::new(Mutex::new(json!([]))), Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let (mut rig, _repo) = live_rig(&base, root.path());
    let id = format!("{}p0", rig.machine.base_scope);
    rig.lp(0).project.repo_path = root.path().join("nowhere").to_string_lossy().into_owned();
    rig.lp(0).file_project.repo_path = root.path().join("nowhere").to_string_lossy().into_owned();
    rig.machine.fetch().await;
    assert!(kinds(&mut rig).contains(&"error".to_string()));
    assert_eq!(crate::base_hold::base_holder(&id), None);
}

fn fact_writes(asked: &Asked) -> Vec<(String, Value)> {
    let asked = asked.lock().unwrap();
    let sent = asked.iter().filter(|(op, _)| op == "fact_set");
    sent.map(|(_, a)| (a["kind"].as_str().unwrap().to_string(), a["data"].clone()))
        .collect()
}

#[tokio::test]
async fn the_last_release_is_kept_for_the_site_once_and_again_only_when_it_changes() {
    let (base, asked) = store(Arc::new(Mutex::new(json!([]))), Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let (mut rig, repo) = live_rig(&base, root.path());
    rig.lp(0).project.base_branch = "main".into();
    let head = push(&repo, "a.txt", "one");
    run(git(&repo).args(["tag", "v1", &head]));
    run(git(&repo).args(["push", "--quiet", "origin", "v1"]));
    rig.machine.fetch().await;
    let writes = fact_writes(&asked);
    assert_eq!(writes.len(), 1, "{writes:?}");
    assert_eq!(writes[0].0, "release_last");
    assert_eq!(writes[0].1["last"]["label"], "v1");
    assert!(writes[0].1["last"]["at"].as_str().unwrap().starts_with("20"));
    // The store's row is read before anything is written, and not again.
    assert_eq!(
        ops(&asked).iter().filter(|o| *o == "facts_get").count(),
        2,
        "{:?}",
        ops(&asked)
    );
    asked.lock().unwrap().clear();
    rig.machine.fetch().await;
    assert!(
        fact_writes(&asked).is_empty() && ops(&asked).is_empty(),
        "{:?}",
        ops(&asked)
    );
}

#[tokio::test]
async fn a_store_that_cant_be_read_gets_no_facts_and_the_next_fetch_tries_again() {
    let root = tempfile::tempdir().unwrap();
    let (mut rig, _repo) = live_rig("http://127.0.0.1:1", root.path());
    rig.machine.fetch().await;
    let (base, asked) = store(Arc::new(Mutex::new(json!([]))), Arc::default()).await;
    rig.lp(0).client = Arc::new(crate::backlog::Client::new(base));
    rig.machine.fetch().await;
    assert_eq!(fact_writes(&asked).len(), 1, "{:?}", ops(&asked));
}

#[tokio::test]
async fn a_fetch_that_moves_main_wakes_the_projects_rollout_loop() {
    let (base, _asked) = store(Arc::new(Mutex::new(json!([]))), Arc::default()).await;
    let root = tempfile::tempdir().unwrap();
    let (mut rig, repo) = live_rig(&base, root.path());
    push(&repo, "src.txt", "one");
    rig.machine.fetch().await;
    let woke = |rig: &Rig| rig.machine.runners.wake("p0");
    let first = tokio::time::timeout(std::time::Duration::from_millis(100), woke(&rig).notified()).await;
    assert!(first.is_ok(), "main moved, so the rollout loop is woken");
    rig.machine.fetch().await;
    let again = tokio::time::timeout(std::time::Duration::from_millis(100), woke(&rig).notified()).await;
    assert!(again.is_err(), "main didn't move");
}
