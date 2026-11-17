//! A chore task as a local item (`driver/chores.rs`): the store's task becomes an item with no store item and no lease, goes
//! through the item flow, merges on a chore entry in the store's queue, and ends its task from where the item ended up. The
//! store hears of it only through the merge queue, `task_end`, and (a fix main that fixed nothing) `chore_nofix`.
use super::super::*;
use crate::orchestrator::steps::{MergeEnd, state_name};
use crate::store::FlowRecord;

const PR: &str = "https://github.com/o/p/pull/9";

/// What the fake store does besides answering: `task_end` fails while `end_down` is set.
#[derive(Default)]
struct Plan {
    tasks: Vec<Value>,
    end_down: bool,
}

async fn store(plan: Arc<Mutex<Plan>>) -> (String, Asked) {
    use axum::http::StatusCode;
    let asked: Asked = Default::default();
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, plan) = (seen.clone(), plan.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                if matches!(op.as_str(), "settings_get" | "pause_get" | "facts_get") {
                    let value = match op.as_str() {
                        "pause_get" => json!({ "paused": null, "log": [] }),
                        "facts_get" => json!({}),
                        _ => Value::Null,
                    };
                    return (StatusCode::OK, Json(json!({ "value": value })));
                }
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let mut plan = plan.lock().unwrap();
                let value = match op.as_str() {
                    "health" => json!({ "tasks": plan.tasks, "stop": [], "free": 1, "allowed": 2 }),
                    "task_end" if plan.end_down => {
                        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" })));
                    }
                    "task_end" => {
                        let ended = body["args"]["id"].clone();
                        plan.tasks.retain(|t| t["id"] != ended);
                        json!({ "held": true })
                    }
                    "merge_next" => json!({ "turn": { "token": "turn-1", "entry": { "item": "task-c" } } }),
                    "merge_beat" | "merge_result" => json!({ "held": true }),
                    "merge_open" | "merge_enqueue" | "merge_withdraw" | "chore_nofix" => json!({}),
                    _ => json!([]),
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

fn task(subject: &str) -> Value {
    json!({ "id": "task-c", "subject": subject, "tier": "fast", "prompt": "Tidy the parser." })
}

async fn live(subject: &str) -> (Rig, Arc<Fake>, Asked, Arc<Mutex<Plan>>) {
    let plan = Arc::new(Mutex::new(Plan {
        tasks: vec![task(subject)],
        ..Plan::default()
    }));
    let (base, asked) = store(plan.clone()).await;
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

fn done(state: ItemState, flow: Option<FlowRecord>) -> StepDone {
    let mut run = ItemRun::new();
    run.state = state;
    StepDone {
        run,
        flow,
        ..Default::default()
    }
}

#[tokio::test]
async fn a_chore_task_runs_the_whole_chain_with_no_lease_and_writes_nothing_to_the_store_but_its_turn_and_its_end() {
    let (mut rig, fake, asked, _plan) = live("chore:tidy").await;
    fake.script
        .lock()
        .unwrap()
        .push(SessionResult::Implementer(ImplementerOutcome::Finished));
    let reviewed = FlowRecord {
        pr_url: Some(PR.into()),
        pr_number: Some(9),
        approved_sha: Some("abc".into()),
        ..Default::default()
    };
    let mut merged = done(ItemState::Merged, None);
    merged.merge = Some(MergeEnd::Merged {
        commit: Some("deadbeef".into()),
    });
    let mut script: std::collections::BTreeMap<&str, StepDone> = [
        (
            "checking",
            done(
                ItemState::Reviewing,
                Some(FlowRecord {
                    pr_url: Some(PR.into()),
                    pr_number: Some(9),
                    ..Default::default()
                }),
            ),
        ),
        ("reviewing", done(ItemState::AwaitingCi, Some(reviewed))),
        ("awaiting_ci", done(ItemState::Merging, None)),
        ("merging", merged),
    ]
    .into();
    let mut seen_states = Vec::new();
    for round in 0..30 {
        let state = rig.lp(0).runs.get("task-c").map(|r| state_name(r.state));
        if let Some(state) = state
            && fake.done.lock().unwrap().is_none()
            && let Some(next) = script.remove(state)
        {
            seen_states.push(state);
            *fake.done.lock().unwrap() = Some(next);
        }
        rig.machine.pass().await;
        settle().await;
        if round == 0 {
            // Started as the store's own words: the task's item, no token, the chore's prompt and kind.
            let started = fake.started.lock().unwrap();
            let request = &started[0];
            assert_eq!((request.item_id.as_str(), request.token.as_str()), ("task-c", ""));
            let work = request.chore.as_ref().unwrap();
            assert_eq!((work.kind.as_str(), work.prompt.as_str()), ("tidy", "Tidy the parser."));
            assert_eq!(
                request.item.tier.as_deref(),
                Some(crate::tiers::SONNET),
                "a fast task is Sonnet"
            );
            assert!(rig.lp(0).leases.is_empty() && rig.lp(0).chores.contains_key("task-c"));
            let kept = crate::store::Store::load(rig.lp(0).store.dir()).unwrap();
            assert!(kept.chores().contains_key("task-c") && kept.runs().contains_key("task-c"));
        }
        if script.is_empty() && !rig.lp(0).runs.contains_key("task-c") {
            break;
        }
    }
    assert_eq!(seen_states, ["checking", "reviewing", "awaiting_ci", "merging"]);
    let ops = super::super::flow::calls(&asked);
    for never in [
        "claim",
        "park",
        "release",
        "complete",
        "tryout_add",
        "details_report",
        "report",
    ] {
        assert!(
            !ops.iter().any(|o| o == never),
            "{never} was written for a chore: {ops:?}"
        );
    }
    let enqueued = &sent(&asked, "merge_enqueue")[0];
    assert_eq!(
        (enqueued["item"].as_str(), enqueued["chore"].as_bool()),
        (Some("task-c"), Some(true))
    );
    assert_eq!(sent(&asked, "merge_result")[0]["outcome"], json!("merged"));
    // The task ended ok once, when the item was handed on (CI), and the merged chore is forgotten, in memory and on disk.
    assert_eq!(sent(&asked, "task_end"), [json!({ "id": "task-c", "ok": true })]);
    let lp = rig.lp(0);
    assert!(lp.runs.is_empty() && lp.chores.is_empty() && lp.items.is_empty());
    let kept = crate::store::Store::load(lp.store.dir()).unwrap();
    assert!(kept.chores().is_empty() && kept.runs().is_empty() && kept.pool_tasks().is_empty());
}

#[tokio::test]
async fn a_chore_that_stops_for_a_person_is_dropped_its_change_closed_and_its_task_ended_not_ok() {
    let (mut rig, fake, asked, _plan) = live("chore:fix_main").await;
    // The session gives up: a chore never waits on a person.
    fake.script
        .lock()
        .unwrap()
        .push(SessionResult::NeedsHuman("The fix made no commit".into()));
    rig.machine.pass().await;
    settle().await;
    assert!(rig.lp(0).chores.contains_key("task-c"));
    // The fix main was queued for main's commit `abc`, which it found nothing to fix at; its change is open.
    rig.lp(0).chores.get_mut("task-c").unwrap().red_sha = Some("abc".into());
    rig.lp(0).records.get_mut("task-c").unwrap().flow.pr_url = Some(PR.into());
    // The fake engine runs a step in `needs_human` (the real flow has none); it is over a pass later.
    rig.machine.pass().await;
    rig.machine.pass().await;
    let abandoned = fake.abandoned.lock().unwrap().clone();
    assert_eq!(abandoned.len(), 1, "{abandoned:?}");
    assert_eq!(abandoned[0].pr_url.as_deref(), Some(PR));
    assert_eq!(sent(&asked, "chore_nofix")[0]["sha"], json!("abc"));
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "task_end"), [json!({ "id": "task-c", "ok": false })]);
    assert!(rig.lp(0).runs.is_empty() && rig.lp(0).chores.is_empty() && rig.lp(0).records.is_empty());
    assert_eq!(rig.machine.pool.lane_of("p0", "task-c"), None, "its lane is free");
}

#[tokio::test]
async fn a_chore_comes_back_after_a_restart_in_its_state_and_its_task_is_not_started_twice() {
    let (mut rig, fake, asked, plan) = live("chore:bugs").await;
    fake.script
        .lock()
        .unwrap()
        .push(SessionResult::Implementer(ImplementerOutcome::Finished));
    rig.machine.pass().await;
    settle().await;
    rig.machine.pass().await;
    assert_eq!(rig.lp(0).runs["task-c"].state, ItemState::Checking);
    let (base, asked2) = store(plan).await;
    drop(asked);
    let mut rig = super::super::restart::restarted(rig, &base);
    let fake = rig.go_live();
    rig.machine.pass().await;
    let lp = rig.lp(0);
    assert_eq!(lp.runs["task-c"].state, ItemState::Checking, "taken back in its state");
    assert!(lp.chores.contains_key("task-c") && lp.leases.is_empty());
    assert!(fake.started.lock().unwrap().is_empty(), "the task is not started again");
    let health = sent(&asked2, "health");
    assert_eq!(
        health[0]["running"],
        json!([{ "id": "task-c" }]),
        "reported running at once"
    );
    assert!(sent(&asked2, "task_end").is_empty());
}

#[tokio::test]
async fn a_task_end_the_store_does_not_hear_is_kept_until_it_does() {
    let (mut rig, _fake, asked, plan) = live("chore:tests").await;
    rig.machine.pass().await;
    plan.lock().unwrap().end_down = true;
    rig.lp(0).runs.get_mut("task-c").unwrap().state = ItemState::AwaitingCi;
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "task_end").len(), 1, "tried while the store is down");
    let kept = crate::store::Store::load(rig.lp(0).store.dir()).unwrap();
    assert_eq!(kept.task_ends_owed()[0].end.id, "task-c");
    assert!(
        rig.lp(0).chores.contains_key("task-c"),
        "the chore carries on beside its end"
    );
    plan.lock().unwrap().end_down = false;
    rig.lp(0).pool.retry_owed_now();
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "task_end").len(), 2, "sent again once the store answers");
}

#[tokio::test]
async fn a_task_sent_again_is_not_a_second_item_and_a_store_that_is_down_sends_no_chore() {
    let (mut up, fake, asked, _plan) = live("chore:tidy").await;
    up.machine.pass().await;
    up.machine.pass().await;
    assert_eq!(fake.started.lock().unwrap().len(), 1);
    assert!(sent(&asked, "task_end").is_empty());
    // `health` is what sends a chore: with the store down none exists.
    let (down, _) = claim_stub(json!([]), None).await;
    let mut cut = rig(2, &[&down]);
    let fake = cut.go_live();
    cut.machine.pass().await;
    assert!(fake.started.lock().unwrap().is_empty() && cut.lp(0).chores.is_empty());
}

/// A project on `team` whose origin has `main` one commit ahead (touching the same file when `conflict`), with a fake `gh`.
async fn behind_main(conflict: bool) -> (Rig, Arc<Fake>, Asked, tempfile::TempDir) {
    use super::super::main_sync::{commit, fake_gh, push};
    use crate::test_support::{git, repo_with_origin, run as run_git};
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    run_git(git(&repo).args(["checkout", "--quiet", "-b", "team"]));
    commit(&repo, "c.txt", "team side");
    push(&repo, "team");
    run_git(git(&repo).args(["checkout", "--quiet", "main"]));
    commit(&repo, if conflict { "c.txt" } else { "m.txt" }, "main side");
    push(&repo, "main");
    let plan = Arc::new(Mutex::new(Plan {
        tasks: vec![task("chore:sync_main:team")],
        ..Plan::default()
    }));
    let (base, asked) = store(plan).await;
    let mut rig = rig(2, &[&base]);
    let fake = rig.go_live();
    let gh = fake_gh(root.path());
    rig.machine.links.env = Env::from_pairs([("RETASKABLE_GH", gh.display().to_string())]);
    let lp = &mut rig.machine.projects[0];
    for p in [&mut lp.project, &mut lp.file_project] {
        p.repo_path = repo.display().to_string();
        p.worktrees_dir = root.path().join("wt").display().to_string();
        p.base_branch = "team".into();
    }
    lp.seen.default_branch = Some("main".into());
    (rig, fake, asked, root)
}

#[tokio::test]
async fn a_main_sync_task_that_merges_clean_opens_its_change_and_waits_for_ci_with_no_session() {
    let (mut rig, fake, asked, _root) = behind_main(false).await;
    rig.machine.pass().await;
    let lp = rig.lp(0);
    let run = lp.runs.get("task-c").copied().expect("the sync's item");
    assert_eq!(run.state, ItemState::AwaitingCi);
    assert_eq!(
        lp.records["task-c"].flow.pr_url.as_deref(),
        Some("https://github.com/o/r/pull/9")
    );
    assert_eq!(lp.records["task-c"].flow.pr_number, Some(9));
    assert!(
        lp.chores["task-c"]
            .sync
            .as_ref()
            .is_some_and(|s| s.conflicts.is_empty())
    );
    assert!(
        fake.started.lock().unwrap().is_empty(),
        "a clean merge needs no session"
    );
    assert!(
        sent(&asked, "task_end").is_empty(),
        "the task ends once the item is handed on, which it is"
    );
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "task_end"), [json!({ "id": "task-c", "ok": true })]);
}

#[tokio::test]
async fn a_main_sync_task_that_conflicts_queues_a_session_on_the_syncs_own_branch() {
    let (mut rig, fake, _asked, _root) = behind_main(true).await;
    rig.machine.pass().await;
    let started = fake.started.lock().unwrap().clone();
    assert_eq!(started.len(), 1, "{:?}", rig.lp(0).runs);
    let work = started[0].chore.as_ref().unwrap();
    assert_eq!(work.kind, "sync_main");
    assert!(work.sync.as_ref().unwrap().conflicts.contains("c.txt"), "{work:?}");
    assert!(
        started[0]
            .item
            .branch
            .as_deref()
            .unwrap()
            .starts_with("retaskable/sync-main-")
    );
}

#[tokio::test]
async fn a_main_sync_task_with_nothing_to_sync_ends_ok_at_once() {
    let (mut rig, fake, asked, _root) = behind_main(false).await;
    rig.lp(0).seen.default_branch = Some("team".into());
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "task_end"), [json!({ "id": "task-c", "ok": true })]);
    assert!(fake.started.lock().unwrap().is_empty() && rig.lp(0).chores.is_empty());
}
