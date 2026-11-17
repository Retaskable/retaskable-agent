//! Pool tasks in claim mode (`driver/pool.rs`): `health` once a minute with `obeys`, the tasks it answers started
//! (an item claimed by name, `chore:specs` as a draft, the rest ended), and `task_end` owed until the store answers.
use super::*;
use crate::pool_tasks::OwedTaskEnd;

mod jobs;
mod smoke;

/// What the fake store does, changed by a test between passes.
#[derive(Default)]
struct Plan {
    /// The tasks `health` sends (until claimed by name or ended).
    tasks: Vec<Value>,
    /// Answers `claim` with this status and error code when set.
    claim_error: Option<(u16, &'static str)>,
    /// `task_end` answers 500 while set.
    end_down: bool,
    /// `health` answers 500 while set.
    health_down: bool,
}

async fn stub(plan: Arc<Mutex<Plan>>) -> (String, Asked) {
    use axum::http::StatusCode;
    let asked: Asked = Default::default();
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, plan) = (seen.clone(), plan.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                if op == "settings_get" || op == "pause_get" || op == "facts_get" {
                    return (StatusCode::OK, Json(json!({ "value": if op == "pause_get" { json!({ "paused": null, "log": [] }) } else if op == "facts_get" { json!({}) } else { Value::Null } })));
                }
                if op == "details_get" {
                    return (StatusCode::OK, Json(json!({ "value": Value::Null })));
                }
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                // The pool ops are checked as the store checks them: a wrong argument is a 400, not a pass.
                if matches!(op.as_str(), "health" | "claim" | "task_end" | "chores_get" | "chore_trigger" | "chore_nofix")
                    && let Err(why) = crate::backlog::pool::contract::check_op(&op, &body["args"])
                {
                    return (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad_request", "message": why })));
                }
                let id = body["args"]["id"].as_str().unwrap_or("a").to_string();
                let mut plan = plan.lock().unwrap();
                match op.as_str() {
                    "health" if plan.health_down => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" }))),
                    "health" => (StatusCode::OK, Json(json!({ "value": { "tasks": plan.tasks, "stop": [], "free": 1, "allowed": 2 } }))),
                    "claim" => match plan.claim_error {
                        Some((status, code)) => (StatusCode::from_u16(status).unwrap(), Json(json!({ "error": code, "message": "no" }))),
                        None => {
                            plan.tasks.retain(|t| t["subject"] != id.as_str());
                            (StatusCode::OK, Json(json!({ "value": { "item": item(&id, "claimed"), "token": format!("lease-{id}") } })))
                        }
                    },
                    "task_end" if plan.end_down => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" }))),
                    "task_end" => {
                        let ended = body["args"]["id"].clone();
                        plan.tasks.retain(|t| t["id"] != ended);
                        (StatusCode::OK, Json(json!({ "value": { "held": true } })))
                    }
                    "draft_queue" => (StatusCode::OK, Json(json!({ "value": { "draft": { "id": "draft-1" } } }))),
                    _ => (StatusCode::OK, Json(json!({ "value": [item("a", "claimed"), item("b", "claimed")] }))),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

fn task(id: &str, subject: &str) -> Value {
    json!({ "id": id, "subject": subject, "tier": "smart", "prompt": "do it" })
}

async fn live(tasks: Vec<Value>) -> (Rig, Arc<Fake>, Asked, Arc<Mutex<Plan>>) {
    let plan = Arc::new(Mutex::new(Plan {
        tasks,
        ..Plan::default()
    }));
    let (base, asked) = stub(plan.clone()).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    (rig, fake, asked, plan)
}

fn sent(asked: &Asked, op: &str) -> Vec<Value> {
    asked
        .lock()
        .unwrap()
        .iter()
        .filter(|(o, _)| o == op)
        .map(|(_, a)| a.clone())
        .collect()
}

#[tokio::test]
async fn health_goes_out_once_a_minute_and_a_wake_sends_it_at_once() {
    let (mut rig, _fake, asked, _plan) = live(vec![]).await;
    rig.lp(0).pool.every = Duration::from_secs(60);
    rig.machine.pass().await;
    rig.machine.pass().await;
    rig.machine.poll().await;
    assert_eq!(sent(&asked, "health").len(), 1, "once a minute, whatever the passes");
    let id = rig.machine.projects[0].id.clone();
    rig.machine.links.work.wake(&id);
    rig.machine.poll().await;
    assert_eq!(sent(&asked, "health").len(), 2, "work_available asks at once");
}

#[tokio::test]
async fn health_says_it_obeys_and_what_runs() {
    let (mut rig, fake, asked, _plan) = live(vec![task("task-1", "a")]).await;
    rig.machine.links.tags = vec!["web".to_string()].into();
    rig.machine.pass().await;
    rig.machine.pass().await;
    let reports = sent(&asked, "health");
    assert_eq!(reports[0]["obeys"], true);
    assert_eq!(reports[0]["running"], json!([]));
    assert_eq!(reports[0]["agent"]["tags"], json!(["web"]));
    assert_eq!(reports[0]["can"], json!([]), "no jobs are run here, so none is claimed");
    assert_eq!(
        reports[1]["running"],
        json!([{ "id": "task-1" }]),
        "a task claimed here is reported running"
    );
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert_eq!(sent(&asked, "claim").len(), 1, "taken once, not again");
}

#[tokio::test]
async fn an_item_task_is_claimed_by_name_and_started_on_the_tier_the_store_picked() {
    let mut fast = task("task-1", "a");
    fast["tier"] = json!("fast");
    let (mut rig, fake, asked, _plan) = live(vec![fast]).await;
    rig.machine.pass().await;
    assert_eq!(ops(&asked)[..3], ["list", "health", "claim"]);
    let claim = &sent(&asked, "claim")[0];
    assert_eq!(claim["id"], "a");
    assert_eq!(claim["agent"]["machine"], "my-pc");
    assert_eq!(
        rig.lp(0).items["a"].tier.as_deref(),
        Some("sonnet"),
        "fast is sonnet at most"
    );
    assert_eq!(fake.started.lock().unwrap()[0].token, "lease-a");
}

#[tokio::test]
async fn a_task_the_store_refuses_for_good_ends_not_ok_and_is_not_tried_again() {
    for code in ["conflict", "not_found", "held_by_owner"] {
        let (mut rig, fake, asked, plan) = live(vec![task("task-1", "a")]).await;
        plan.lock().unwrap().claim_error = Some((409, code));
        rig.machine.pass().await;
        rig.machine.pass().await;
        assert_eq!(sent(&asked, "claim").len(), 1, "{code}: not tried again");
        assert_eq!(
            sent(&asked, "task_end")[0],
            json!({ "id": "task-1", "ok": false }),
            "{code}"
        );
        assert!(fake.started.lock().unwrap().is_empty() && rig.lp(0).runs.is_empty());
    }
}

#[tokio::test]
async fn a_task_that_cannot_be_claimed_for_now_comes_again_with_the_next_report() {
    let (mut rig, fake, asked, plan) = live(vec![task("task-1", "a")]).await;
    plan.lock().unwrap().claim_error = Some((500, "boom"));
    rig.machine.pass().await;
    assert!(
        sent(&asked, "task_end").is_empty(),
        "no end: it is retried, not given up"
    );
    plan.lock().unwrap().claim_error = None;
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "claim").len(), 2);
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_store_that_cannot_be_reached_starts_nothing() {
    let (mut rig, fake, asked, plan) = live(vec![task("task-1", "a")]).await;
    plan.lock().unwrap().health_down = true;
    rig.machine.pass().await;
    assert_eq!(ops(&asked), ["list", "health"]);
    assert!(fake.started.lock().unwrap().is_empty() && rig.lp(0).runs.is_empty());
    assert_eq!(kinds(&mut rig), ["store_error", "claim", "tick"]);
}

#[tokio::test]
async fn jobs_this_agent_does_not_run_and_chores_it_does_not_know_are_ended_at_once_in_words() {
    let mut job = task("task-3", "job:lighthouse");
    job["job"] = json!({ "kind": "lighthouse", "day": "2026-11-17" });
    let tasks = vec![job, task("task-4", "chore:juggle")];
    let (mut rig, fake, asked, _plan) = live(tasks).await;
    rig.machine.pass().await;
    let ends = sent(&asked, "task_end");
    assert_eq!(ends.len(), 2);
    assert!(ends.iter().all(|e| e["ok"] == false));
    assert!(ends[0]["refused"]["why"].as_str().unwrap().contains("lighthouse"));
    assert!(ends[1]["note"].as_str().unwrap().contains("juggle"));
    assert!(sent(&asked, "claim").is_empty() && fake.started.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_specs_chore_queues_a_spec_draft_and_ends_ok() {
    let (mut rig, _fake, asked, _plan) = live(vec![task("task-1", "chore:specs")]).await;
    rig.machine.pass().await;
    let draft = &sent(&asked, "draft_queue")[0];
    assert_eq!(draft["request"], "do it");
    assert_eq!(draft["chore"], "specs");
    assert_eq!(sent(&asked, "task_end")[0], json!({ "id": "task-1", "ok": true }));
}

#[tokio::test]
async fn a_task_ends_when_its_items_chain_is_handed_on_and_not_before() {
    let (mut rig, _fake, asked, _plan) = live(vec![task("task-1", "a")]).await;
    rig.machine.pass().await;
    assert!(
        sent(&asked, "task_end").is_empty(),
        "queued and implementing are still the task"
    );
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::AwaitingCi;
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "task_end"), [json!({ "id": "task-1", "ok": true })]);
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "task_end").len(), 1, "sent once");
    assert_eq!(sent(&asked, "health").last().unwrap()["running"], json!([]));
}

#[tokio::test]
async fn an_item_that_stops_for_a_person_ends_its_task_not_ok() {
    let (mut rig, _fake, asked, _plan) = live(vec![task("task-1", "a")]).await;
    rig.machine.pass().await;
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::NeedsHuman;
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "task_end"), [json!({ "id": "task-1", "ok": false })]);
}

#[tokio::test]
async fn an_end_the_store_did_not_hear_is_kept_across_a_restart_and_sent_when_it_is_back() {
    let (mut rig, _fake, asked, plan) = live(vec![task("task-1", "a")]).await;
    rig.machine.pass().await;
    plan.lock().unwrap().end_down = true;
    rig.lp(0).runs.get_mut("a").unwrap().state = ItemState::Merging;
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "task_end").len(), 1, "tried once while the store is down");
    // The end is on disk before it is sent: a restart reads it back with the running tasks.
    let kept = crate::store::Store::load(rig.lp(0).store.dir()).unwrap();
    assert_eq!(kept.task_ends_owed().len(), 1);
    assert_eq!(kept.task_ends_owed()[0].end.id, "task-1");
    assert!(kept.pool_tasks().is_empty(), "the task no longer runs");
    let restored = super::super::pool::Pool::restoring(&kept);
    rig.lp(0).pool = restored;
    rig.lp(0).pool.every = Duration::ZERO;
    plan.lock().unwrap().end_down = false;
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "task_end").len(), 2, "sent again once the store answers");
    assert!(
        crate::store::Store::load(rig.lp(0).store.dir())
            .unwrap()
            .task_ends_owed()
            .is_empty()
    );
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "task_end").len(), 2, "and not again");
}

#[tokio::test]
async fn a_restart_reports_the_tasks_it_still_runs_and_does_not_start_them_twice() {
    let (mut rig, fake, asked, _plan) = live(vec![task("task-1", "a")]).await;
    rig.machine.pass().await;
    let kept = crate::store::Store::load(rig.lp(0).store.dir()).unwrap();
    assert_eq!(kept.pool_tasks()["task-1"].item, "a");
    rig.lp(0).pool = super::super::pool::Pool::restoring(&kept);
    rig.lp(0).pool.every = Duration::ZERO;
    rig.machine.pass().await;
    assert_eq!(
        sent(&asked, "health").last().unwrap()["running"],
        json!([{ "id": "task-1" }])
    );
    assert_eq!(sent(&asked, "claim").len(), 1);
    assert_eq!(fake.started.lock().unwrap().len(), 1);
}

#[test]
fn an_owed_end_is_what_the_state_file_keeps() {
    let owed = OwedTaskEnd {
        end: crate::backlog::pool::TaskEnd::done("t", true),
        since: 5,
    };
    assert_eq!(serde_json::to_value(&owed).unwrap()["end"]["id"], "t");
}
