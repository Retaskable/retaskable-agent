//! A claimed item's implementer says `nothing_left` or `waits_on` (`driver/nothing_left.rs`; `nothingLeft` and `waitOn`
//! in the TS agent's `steps.ts`, `ciWhenReady.test.ts` and `steps.test.ts`).
use super::*;
use crate::orchestrator::driver::finish_steps::FinishHost;
use axum::http::StatusCode;
use futures_util::future::BoxFuture;
use std::path::PathBuf;

/// A host that reads the base head (or fails to) and records the changes it is asked to close.
struct Host {
    head: Result<(String, bool), String>,
    closed: Mutex<Vec<(String, String)>>,
    close_fails: bool,
}

impl FinishHost for Host {
    fn base_head(&self, _: PathBuf, _: String, _: String) -> BoxFuture<'static, Result<(String, bool)>> {
        let head = self.head.clone().map_err(|e| anyhow::anyhow!(e));
        Box::pin(async move { head })
    }

    fn close_change(&self, _: PathBuf, branch: String, comment: String) -> BoxFuture<'static, Result<bool>> {
        self.closed.lock().unwrap().push((branch, comment));
        let fails = self.close_fails;
        Box::pin(async move {
            if fails {
                anyhow::bail!("gh is down");
            }
            Ok(true)
        })
    }
}

fn host(head: Result<(String, bool), String>, close_fails: bool) -> Arc<Host> {
    Arc::new(Host {
        head,
        closed: Default::default(),
        close_fails,
    })
}

/// A store that hands out "a" (lease-1, the item as `listed`) and takes `complete`, `depends`, `report` and `release`;
/// `depends` refuses the id `nope`, and `complete` answers `complete_status`.
async fn store(listed: Value, complete_status: u16) -> (String, Asked) {
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let served = Arc::new(AtomicBool::new(false));
    let details: Details = Default::default();
    let claim = json!({ "item": listed.clone(), "token": "lease-1" });
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, listed, claim, served, details) = (
                seen.clone(),
                listed.clone(),
                claim.clone(),
                served.clone(),
                details.clone(),
            );
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                if let Some(answer) = details_answer(&details, &op, &body["args"]) {
                    return answer;
                }
                if matches!(op.as_str(), "settings_get" | "pause_get" | "facts_get") {
                    let value = match op.as_str() {
                        "pause_get" => json!({ "paused": null, "log": [] }),
                        "facts_get" => json!({}),
                        _ => Value::Null,
                    };
                    return (StatusCode::OK, Json(json!({ "value": value })));
                }
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = match op.as_str() {
                    "health" => {
                        let tasks = if served.load(Ordering::SeqCst) {
                            json!([])
                        } else {
                            starter::tasks_of(&claim)
                        };
                        json!({ "tasks": tasks, "stop": [], "free": 1, "allowed": 2,
                            "tuning": crate::store_tuning::StoreTuning::test_json() })
                    }
                    "claim" => {
                        served.store(true, Ordering::SeqCst);
                        claim.clone()
                    }
                    "complete" if complete_status != 200 => {
                        return (
                            StatusCode::from_u16(complete_status).unwrap(),
                            Json(json!({ "error": "boom" })),
                        );
                    }
                    "complete" => json!({ "built": true, "status": "merged" }),
                    "depends" if body["args"]["add"] == json!(["nope"]) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(json!({ "error": "bad_request", "message": "nope is not an item" })),
                        );
                    }
                    "depends" => json!({ "dependsOn": body["args"]["add"] }),
                    "release" | "park" | "report" => json!({}),
                    _ => json!([listed]),
                };
                (StatusCode::OK, Json(json!({ "value": value })))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

fn calls_to(asked: &Asked, op: &str) -> Vec<Value> {
    (asked.lock().unwrap().iter())
        .filter(|(o, _)| o == op)
        .map(|(_, a)| a.clone())
        .collect()
}

/// A rig that claimed "a" on lane 1, whose session ends as `result`, settled.
async fn ended(listed: Value, host: Arc<Host>, complete_status: u16, result: SessionResult) -> (Rig, Asked) {
    let (base, asked) = store(listed, complete_status).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).project.base_branch = "main".into();
    rig.machine.finish_host = Some(host);
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(result);
    rig.machine.pass().await;
    rig.machine.pass().await;
    (rig, asked)
}

fn built() -> Result<(String, bool), String> {
    Ok(("abc123".into(), true))
}

fn nothing_left(why: &str) -> SessionResult {
    SessionResult::NothingLeft(why.into())
}

#[tokio::test]
async fn nothing_left_closes_the_open_change_frees_the_lane_and_completes_at_the_base_head() {
    let mut listed = item("a", "claimed");
    listed["branch"] = json!("retaskable/a-s1");
    listed["prUrl"] = json!("https://github.com/o/r/pull/739");
    let host = host(built(), false);
    let why = "Connected apps is built by mcp-connected-apps.";
    let (mut rig, asked) = ended(listed, host.clone(), 200, nothing_left(why)).await;
    assert_eq!(
        host.closed.lock().unwrap().clone(),
        vec![(
            "retaskable/a-a-s0".to_string(),
            format!("Retaskable closed this: nothing is left to build. {why}")
        )]
    );
    let done = calls_to(&asked, "complete");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["id"], "a");
    assert_eq!(done[0]["token"], "lease-1");
    assert_eq!(done[0]["built"], true);
    assert_eq!(done[0]["mergeCommit"], "abc123");
    assert_eq!(done[0]["onDefaultBranch"], true);
    assert_eq!(done[0]["remaining"], "");
    assert!(calls_to(&asked, "release").is_empty(), "completed, not given back");
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None, "the lane is freed");
    let lp = rig.lp(0);
    assert!(
        lp.runs.is_empty() && lp.leases.is_empty() && lp.items.is_empty(),
        "the run and its lease are gone"
    );
    let events = lp.events().recent();
    let line = events
        .iter()
        .filter_map(|e| match e.event() {
            Some(crate::telemetry::Event::ItemState(s)) => s.note.clone(),
            _ => None,
        })
        .find(|n| n.starts_with("Nothing left to build"))
        .expect("the timeline says so");
    assert_eq!(line, format!("Nothing left to build, so PR #739 was closed: {why}"));
}

#[tokio::test]
async fn nothing_left_with_no_change_open_only_completes() {
    let host = host(built(), false);
    let (mut rig, asked) = ended(
        item("a", "claimed"),
        host.clone(),
        200,
        nothing_left("It's all on main."),
    )
    .await;
    // Asked to close the lane's branch's change, the host finds none: the item is built all the same.
    assert_eq!(host.closed.lock().unwrap().len(), 1);
    assert_eq!(calls_to(&asked, "complete").len(), 1);
    assert!(rig.lp(0).runs.is_empty());
}

#[tokio::test]
async fn a_change_that_wont_close_doesnt_stop_the_item_being_built() {
    let mut listed = item("a", "claimed");
    listed["branch"] = json!("retaskable/a-s1");
    let (mut rig, asked) = ended(listed, host(built(), true), 200, nothing_left("Built.")).await;
    assert_eq!(calls_to(&asked, "complete").len(), 1);
    assert!(rig.lp(0).runs.is_empty());
}

#[tokio::test]
async fn nothing_left_with_no_readable_base_head_goes_to_a_person_and_closes_nothing() {
    let mut listed = item("a", "claimed");
    listed["branch"] = json!("retaskable/a-s1");
    let host = host(Err("offline".into()), false);
    let (mut rig, asked) = ended(listed, host.clone(), 200, nothing_left("Built.")).await;
    assert!(host.closed.lock().unwrap().is_empty());
    assert!(calls_to(&asked, "complete").is_empty());
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    let base = rig.lp(0).project.base_branch.clone();
    assert_eq!(
        rig.lp(0).records["a"].needs_human_reason.clone().unwrap(),
        format!("Nothing left to build, but {base}'s head couldn't be read: Built.")
    );
    assert_eq!(rig.lp(0).leases["a"], "lease-1", "still held");
}

#[tokio::test]
async fn nothing_left_on_an_item_with_no_lease_is_marked_by_a_person() {
    let (base, asked) = store(item("a", "claimed"), 200).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).project.base_branch = "main".into();
    rig.machine.finish_host = Some(host(built(), false));
    let fake = rig.go_live();
    fake.script.lock().unwrap().push(nothing_left("It's all on main."));
    rig.machine.pass().await;
    rig.lp(0).leases.remove("a");
    rig.machine.pass().await;
    assert!(calls_to(&asked, "complete").is_empty());
    assert_eq!(
        rig.lp(0).records["a"].needs_human_reason.as_deref(),
        Some("Nothing left to build, so mark it done: It's all on main.")
    );
}

#[tokio::test]
async fn a_store_that_wont_take_the_completion_leaves_it_to_a_person_and_keeps_it_listed() {
    let (mut rig, asked) = ended(item("a", "claimed"), host(built(), false), 500, nothing_left("Built.")).await;
    assert_eq!(calls_to(&asked, "complete").len(), 1);
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    let why = rig.lp(0).records["a"].needs_human_reason.clone().unwrap();
    assert!(
        why.starts_with("Nothing left to build, but the shared backlog didn't take it"),
        "{why}"
    );
    assert!(why.ends_with(": Built."), "{why}");
    assert!(rig.lp(0).outbox.contains_key("a"), "sent again on the next pass");
}

#[tokio::test]
async fn waits_on_tells_the_store_and_gives_the_item_back_with_the_summary() {
    let ids = vec!["store".to_string(), " store".to_string(), String::new()];
    let waits = SessionResult::WaitsOn {
        ids,
        summary: "It needs the store's table.".into(),
    };
    let (mut rig, asked) = ended(item("a", "claimed"), host(built(), false), 200, waits).await;
    assert_eq!(
        calls_to(&asked, "depends"),
        vec![json!({ "id": "a", "add": ["store"] })]
    );
    let note = calls_to(&asked, "report");
    assert!(
        note.contains(&json!({ "id": "a", "token": "lease-1", "note": "Waits for store to be built: It needs the store's table." })),
        "{note:?}"
    );
    assert_eq!(
        calls_to(&asked, "release"),
        vec![json!({ "id": "a", "token": "lease-1" })]
    );
    assert_eq!(rig.machine.pool.lane_of("p0", "a"), None);
    assert!(rig.lp(0).runs.is_empty() && rig.lp(0).leases.is_empty());
    assert!(
        rig.machine.own_failure.is_none(),
        "not this computer's own failure: claiming isn't paused"
    );
}

#[tokio::test]
async fn waits_on_a_refused_id_goes_to_a_person() {
    let waits = SessionResult::WaitsOn {
        ids: vec!["nope".into()],
        summary: "It needs the store's table.".into(),
    };
    let (mut rig, asked) = ended(item("a", "claimed"), host(built(), false), 200, waits).await;
    assert!(calls_to(&asked, "release").is_empty(), "not given back");
    assert_eq!(rig.lp(0).runs["a"].state, ItemState::NeedsHuman);
    let why = rig.lp(0).records["a"].needs_human_reason.clone().unwrap();
    assert!(
        why.starts_with("The implementer found this item needs nope built first, but the backlog refused it ("),
        "{why}"
    );
    assert!(why.ends_with("): It needs the store's table."), "{why}");
}

#[tokio::test]
async fn waits_on_with_no_item_named_or_a_change_open_goes_to_a_person_and_the_store_isnt_asked() {
    let none = SessionResult::WaitsOn {
        ids: vec![" ".into()],
        summary: "s".into(),
    };
    let (mut rig, asked) = ended(item("a", "claimed"), host(built(), false), 200, none).await;
    assert!(calls_to(&asked, "depends").is_empty());
    assert_eq!(
        rig.lp(0).records["a"].needs_human_reason.as_deref(),
        Some("The implementer found this item needs another item built first: s")
    );
    let mut listed = item("a", "claimed");
    listed["prUrl"] = json!("https://github.com/o/r/pull/7");
    let open = SessionResult::WaitsOn {
        ids: vec!["store".into()],
        summary: "s".into(),
    };
    let (mut rig, asked) = ended(listed, host(built(), false), 200, open).await;
    assert!(calls_to(&asked, "depends").is_empty());
    assert_eq!(
        rig.lp(0).records["a"].needs_human_reason.as_deref(),
        Some("The implementer found this item needs store built first: s")
    );
}
