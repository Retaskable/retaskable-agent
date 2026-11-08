//! The minute pass adding removal items for deprecated specs.
use super::*;
use axum::response::IntoResponse;
use std::process::Command;

type Seen = Arc<Mutex<Vec<Value>>>;

const SPEC: &str =
    "# Mobile — Spec\n\nDeprecated: 2026-09-01. Remove after: 2026-10-01. Why: dropped.\n\n## 1. Intro\n";

/// How the fake store answers.
#[derive(Clone, Default)]
struct Mood {
    day: &'static str,
    existing: Vec<&'static str>,
    held_by_other: bool,
    conflict: bool,
    no_day: bool,
    fail_add: Arc<AtomicBool>,
}

async fn store(mood: Mood) -> (String, Seen) {
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (log, mood) = (log.clone(), mood.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                log.lock().unwrap().push(body.clone());
                let value = match op.as_str() {
                    "jobs_due" if mood.no_day => {
                        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))).into_response();
                    }
                    "jobs_due" => json!({ "day": mood.day, "due": [] }),
                    "add" if mood.fail_add.load(Ordering::SeqCst) => {
                        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({}))).into_response();
                    }
                    "list" => json!(
                        mood.existing
                            .iter()
                            .enumerate()
                            .map(|(i, id)| json!({ "id": id, "title": "t", "status": "todo", "rank": i }))
                            .collect::<Vec<_>>()
                    ),
                    "lock_take" if mood.held_by_other => json!({ "held": false, "by": "other-pc" }),
                    "lock_take" => json!({ "held": true, "token": "tok1" }),
                    "chore_lock" if mood.held_by_other => json!({ "held": false, "by": "other-pc" }),
                    "chore_lock" => json!({ "held": body["args"]["take"], "by": "my-pc" }),
                    "add" if mood.conflict => {
                        return (axum::http::StatusCode::CONFLICT, Json(json!({ "error": "conflict" })))
                            .into_response();
                    }
                    _ => Value::Null,
                };
                Json(json!({ "value": value })).into_response()
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), seen)
}

/// A repo whose `origin/main` has `docs/specs/mobile.md`.
fn rig_with_spec(base: &str, flag: bool, runs: bool) -> Rig {
    let mut rig = rig(2, &[base]);
    rig.go_live();
    let repo = rig._dir.path().join("repo");
    std::fs::create_dir_all(repo.join("docs/specs")).unwrap();
    std::fs::write(repo.join("docs/specs/mobile.md"), SPEC).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec!["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "specs"],
        vec!["update-ref", "refs/remotes/origin/main", "HEAD"],
    ] {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
    }
    rig.lp(0).project.repo_path = repo.to_string_lossy().into_owned();
    rig.lp(0).project.base_branch = "main".into();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(if flag {
        &["chore-deprecated-specs"]
    } else {
        &[]
    }));
    if runs {
        rig.lp(0).mode = OrchestratorMode::Runs;
    }
    rig
}

async fn minute(rig: &mut Rig) {
    let steps = rig.machine.pass().await;
    rig.machine.deprecation_checks(&steps).await;
    for _ in 0..200 {
        if !rig.lp(0).deprecations_running.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

fn ops(seen: &Seen, names: &[&str]) -> Vec<Value> {
    (seen.lock().unwrap().iter())
        .filter(|b| names.contains(&b["op"].as_str().unwrap_or("")))
        .cloned()
        .collect()
}

fn names(seen: &Seen, names: &[&str]) -> Vec<String> {
    ops(seen, names)
        .iter()
        .map(|b| b["op"].as_str().unwrap().to_string())
        .collect()
}

/// The pass reads the backlog with `list` once a minute too: `lists` counts the check's own, given the minutes run.
fn lists(seen: &Seen, minutes: usize) -> usize {
    names(seen, &["list"]).len() - minutes
}

const MINE: &[&str] = &["add", "chore_lock", "lock_take", "lock_drop"];

#[tokio::test]
async fn a_passed_date_adds_one_removal_item_under_its_lock_and_the_same_day_adds_no_more() {
    let (base, seen) = store(Mood {
        day: "2026-10-02",
        ..Default::default()
    })
    .await;
    let mut rig = rig_with_spec(&base, true, false);
    minute(&mut rig).await;
    assert_eq!(names(&seen, MINE), ["chore_lock", "add", "chore_lock"]);
    let locks = ops(&seen, &["chore_lock"]);
    assert_eq!(locks[0]["args"], json!({ "name": "deprecation-mobile", "take": true }));
    assert_eq!(locks[1]["args"]["take"], false);
    let add = &ops(&seen, &["add"])[0]["args"];
    assert_eq!(add["via"], "deprecation");
    assert_eq!(add["items"][0]["id"], "remove-mobile");
    rig.lp(0).deprecations.lock().unwrap().forget_asker();
    minute(&mut rig).await;
    assert_eq!((lists(&seen, 2), names(&seen, &["add"]).len()), (1, 1));
}

#[tokio::test]
async fn in_runs_mode_the_lock_is_taken_with_a_note_and_dropped_while_paused() {
    let (base, seen) = store(Mood {
        day: "2026-10-02",
        ..Default::default()
    })
    .await;
    let mut rig = rig_with_spec(&base, true, true);
    rig.pause("p0", true);
    minute(&mut rig).await;
    assert_eq!(names(&seen, MINE), ["lock_take", "add", "lock_drop"]);
    let take = &ops(&seen, &["lock_take"])[0]["args"];
    assert_eq!(
        take,
        &json!({ "name": "deprecation-mobile", "note": "deprecation on my-pc" })
    );
    assert_eq!(ops(&seen, &["lock_drop"])[0]["args"]["token"], "tok1");
}

#[tokio::test]
async fn nothing_is_added_when_not_due_existing_held_flag_off_or_no_day() {
    let cases = [
        Mood {
            day: "2026-10-01",
            ..Default::default()
        },
        Mood {
            day: "2026-10-02",
            existing: vec!["remove-mobile"],
            ..Default::default()
        },
        Mood {
            day: "2026-10-02",
            held_by_other: true,
            ..Default::default()
        },
        Mood {
            day: "2026-10-02",
            no_day: true,
            ..Default::default()
        },
    ];
    for mood in cases {
        let (base, seen) = store(mood).await;
        let mut rig = rig_with_spec(&base, true, false);
        minute(&mut rig).await;
        assert!(ops(&seen, &["add"]).is_empty());
        assert!(ops(&seen, &["lock_drop"]).is_empty());
    }
    let (base, seen) = store(Mood {
        day: "2026-10-02",
        ..Default::default()
    })
    .await;
    let mut rig = rig_with_spec(&base, false, false);
    minute(&mut rig).await;
    assert!(names(&seen, &["jobs_due", "add", "chore_lock"]).is_empty());
}

#[tokio::test]
async fn a_conflict_on_add_counts_as_done() {
    let (base, seen) = store(Mood {
        day: "2026-10-02",
        conflict: true,
        ..Default::default()
    })
    .await;
    let mut rig = rig_with_spec(&base, true, false);
    minute(&mut rig).await;
    rig.lp(0).deprecations.lock().unwrap().forget_asker();
    minute(&mut rig).await;
    assert_eq!(names(&seen, &["add"]).len(), 1);
    assert_eq!(names(&seen, &["chore_lock"]), ["chore_lock", "chore_lock"]);
}

#[tokio::test]
async fn a_failed_check_is_retried_only_after_ten_minutes() {
    let fail = Arc::new(AtomicBool::new(true));
    let mood = Mood {
        day: "2026-10-02",
        fail_add: fail.clone(),
        ..Default::default()
    };
    let (base, seen) = store(mood).await;
    let mut rig = rig_with_spec(&base, true, false);
    minute(&mut rig).await;
    assert_eq!(names(&seen, &["add"]).len(), 1);
    fail.store(false, Ordering::SeqCst);
    rig.lp(0).deprecations.lock().unwrap().forget_asker();
    minute(&mut rig).await;
    assert_eq!(names(&seen, &["add"]).len(), 1, "waits for the retry time");
    rig.lp(0).deprecations.lock().unwrap().forget_retry();
    minute(&mut rig).await;
    assert_eq!(names(&seen, &["add"]).len(), 2, "retried");
}
