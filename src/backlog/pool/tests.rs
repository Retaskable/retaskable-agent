use super::*;
use crate::backlog::pool::contract::fake_store;
use crate::backlog::tests::store;
use crate::backlog::week::WeekUse;
use serde_json::json;

fn agent() -> ClaimNextAgent {
    ClaimNextAgent {
        machine: "box-1".into(),
        environment: "retaskable.com".into(),
        base_branch: "dev".into(),
        tags: vec!["web".into()],
        week: Some(WeekUse {
            used: 0.4,
            resets_at: 1_900_000_000_000,
        }),
    }
}

fn report() -> Health {
    Health {
        obeys: true,
        can: vec![],
        project: "demo".into(),
        agent: agent(),
        running: vec![
            RunningTask {
                cost_usd: Some(0.5),
                ..RunningTask::running("task-1".into())
            },
            RunningTask::running("task-2".into()),
        ],
        usage: Some(PoolUsage {
            status: "allowed".into(),
            utilization: Some(0.25),
            resets_at: Some(1_900_000_000),
            week_utilization: None,
            week_resets_at: None,
        }),
        limits: BTreeMap::from([("claude".to_string(), 1_900_000_000_000)]),
        spend: BTreeMap::from([("demo".to_string(), 1.5)]),
        refused: None,
        paused: false,
        draining: false,
        own_failure: None,
    }
}

#[tokio::test]
async fn health_sends_obeys_and_what_runs_and_reads_the_tasks_back() {
    let (base, seen) = fake_store(
        json!({ "value": {
            "tasks": [
                { "id": "task-3", "subject": "a-1", "tier": "fast", "prompt": "Build a-1: A", "state": "sent", "attempt": 1, "agentId": "7" },
                { "id": "task-4", "subject": "chore:fix_main", "prompt": "Fix it", "extraSlot": true },
                { "id": "task-5", "subject": "fix_playtest:k", "tier": "wat", "fix": { "title": "T", "itemId": "a-1" } },
                { "id": "task-6", "subject": "job:x", "job": { "kind": "lighthouse", "day": "2026-11-17" } },
            ],
            "stop": ["task-9"], "free": 1, "allowed": 2, "wait": "the 5-hour window is nearly used up (80% left)",
        } }),
    )
    .await;
    let answer = Client::new(base).health(7, Some("tok"), &report()).await.unwrap();
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "health");
    let args = &sent["args"];
    assert_eq!(args["obeys"], true);
    assert_eq!(args["can"], json!([]));
    assert_eq!(args["project"], "demo");
    assert_eq!(args["agent"]["machine"], "box-1");
    assert_eq!(args["agent"]["baseBranch"], "dev");
    assert_eq!(args["agent"]["week"]["used"], 0.4);
    assert_eq!(
        args["running"],
        json!([{ "id": "task-1", "costUsd": 0.5 }, { "id": "task-2" }])
    );
    assert_eq!(args["usage"]["status"], "allowed");
    assert_eq!(args["usage"]["utilization"], 0.25);
    assert_eq!(args["limits"], json!({ "claude": 1_900_000_000_000_i64 }));
    assert_eq!(args["spend"], json!({ "demo": 1.5 }));
    assert_eq!(
        (args["paused"].clone(), args["draining"].clone()),
        (json!(false), json!(false))
    );
    assert!(args.get("refused").is_none(), "left out when there is none");

    assert_eq!(answer.tasks.len(), 4);
    assert_eq!(answer.tasks[0].subject, "a-1");
    assert_eq!(answer.tasks[0].tier, Tier::Fast);
    assert_eq!(answer.tasks[1].tier, Tier::Smart, "missing reads as smart");
    assert!(answer.tasks[1].extra_slot);
    assert_eq!(answer.tasks[2].tier, Tier::Smart, "an unknown tier reads as smart");
    assert_eq!(answer.tasks[2].fix.as_ref().unwrap().item_id.as_deref(), Some("a-1"));
    assert_eq!(answer.tasks[3].job.as_ref().unwrap().kind, "lighthouse");
    assert_eq!(
        (answer.stop, answer.free, answer.allowed),
        (vec!["task-9".to_string()], 1, 2)
    );
    assert!(answer.wait.unwrap().starts_with("the 5-hour window"));
}

#[tokio::test]
async fn health_with_a_refusal_and_no_usage_leaves_usage_out() {
    let (base, seen) = fake_store(json!({ "value": { "tasks": [], "stop": [], "free": 0, "allowed": 1 } })).await;
    let mut r = report();
    r.usage = None;
    r.refused = Some("signed out".into());
    r.paused = true;
    let answer = Client::new(base).health(7, Some("tok"), &r).await.unwrap();
    let args = seen.lock().unwrap()[0]["args"].clone();
    assert!(args.get("usage").is_none());
    assert_eq!(args["refused"], "signed out");
    assert_eq!(args["paused"], true);
    assert!(answer.tasks.is_empty() && answer.wait.is_none());
}

#[tokio::test]
async fn health_that_cannot_be_asked_or_read_is_an_error_never_an_empty_answer() {
    let (base, _) = fake_store(json!({ "value": "nope" })).await;
    assert!(Client::new(base).health(7, Some("tok"), &report()).await.is_err());
    let (base, _) = fake_store(json!({})).await;
    assert!(Client::new(base).health(7, Some("tok"), &report()).await.is_err());
    let err = Client::new("http://127.0.0.1:9")
        .health(7, Some("tok"), &report())
        .await
        .unwrap_err();
    assert!(matches!(err, BacklogError::Unreachable(_)));
}

#[tokio::test]
async fn task_end_sends_the_id_and_ok_and_reads_held() {
    let (base, seen) = fake_store(json!({ "value": { "held": true } })).await;
    let client = Client::new(base);
    assert!(
        client
            .task_end(7, Some("tok"), &TaskEnd::done("task-3", true))
            .await
            .unwrap()
            .held
    );
    let args = seen.lock().unwrap()[0]["args"].clone();
    assert_eq!(args, json!({ "id": "task-3", "ok": true }));

    let (base, seen) = fake_store(json!({ "value": { "held": false } })).await;
    let end = TaskEnd::refused("task-4", "This computer doesn't run lighthouse jobs.");
    assert!(!Client::new(base).task_end(7, None, &end).await.unwrap().held);
    let args = seen.lock().unwrap()[0]["args"].clone();
    assert_eq!(args["refused"]["why"], "This computer doesn't run lighthouse jobs.");
    assert_eq!(args["ok"], false);
}

#[tokio::test]
async fn task_end_the_store_refuses_keeps_its_code() {
    let (base, _) = store(
        403,
        json!({ "error": "forbidden", "message": "That task is another computer's" }),
    )
    .await;
    let err = Client::new(base)
        .task_end(7, Some("tok"), &TaskEnd::done("task-3", true))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("forbidden"));
    assert!(err.is_refusal());
}

#[tokio::test]
async fn claim_task_names_the_item_and_this_computer_and_reads_its_own_lease() {
    let (base, seen) = fake_store(
        json!({ "value": { "item": { "id": "a-1", "title": "A", "status": "claimed", "rank": 0 }, "token": "lease-1" } }),
    )
    .await;
    let claimed = Client::new(base)
        .claim_task(7, Some("tok"), "a-1", &agent())
        .await
        .unwrap();
    assert_eq!((claimed.item.id.as_str(), claimed.token.as_str()), ("a-1", "lease-1"));
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "claim");
    assert_eq!(sent["args"]["id"], "a-1");
    assert_eq!(sent["args"]["agent"]["machine"], "box-1");
    assert!(sent["args"].get("because").is_none(), "not a person's claim");
}

#[tokio::test]
async fn claim_task_refusals_say_which_one() {
    for code in ["conflict", "not_found", "held_by_owner"] {
        let (base, _) = store(409, json!({ "error": code, "message": "no" })).await;
        let err = Client::new(base)
            .claim_task(7, Some("tok"), "a-1", &agent())
            .await
            .unwrap_err();
        assert_eq!(err.code(), Some(code));
    }
}

fn red_main() -> Trigger {
    Trigger::RedMain {
        branch: "dev".into(),
        sha: "abc1234".into(),
        checks: vec!["npm test".into(), "lint".into()],
        brief: "### npm test\nfailed".into(),
    }
}

#[tokio::test]
async fn red_main_is_sent_under_data_with_the_check_names() {
    let (base, seen) = fake_store(json!({ "value": { "queued": "chore:fix_main" } })).await;
    let answer = Client::new(base)
        .chore_trigger(7, Some("tok"), &red_main())
        .await
        .unwrap();
    assert_eq!(answer, TriggerAnswer::Queued);
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "chore_trigger");
    assert_eq!(
        sent["args"],
        json!({ "trigger": "red_main", "data": { "branch": "dev", "sha": "abc1234", "checks": [{ "name": "npm test" }, { "name": "lint" }], "brief": "### npm test\nfailed" } })
    );
}

#[tokio::test]
async fn a_skipped_trigger_says_why() {
    let (base, _) = fake_store(json!({ "value": { "skipped": "a fix is already open" } })).await;
    let answer = Client::new(base)
        .chore_trigger(7, Some("tok"), &red_main())
        .await
        .unwrap();
    assert_eq!(answer, TriggerAnswer::Skipped("a fix is already open".into()));
}

#[tokio::test]
async fn playtest_fix_smoke_and_behind_main_have_the_stores_shapes() {
    let triggers = [
        Trigger::PlaytestFix {
            key: "a-1@2026".into(),
            item_id: "a-1".into(),
            title: "Jump".into(),
            what: "The jump".into(),
            steps: vec!["Press jump".into(), "  ".into()],
            notes: "It floats".into(),
            pr_url: Some("https://github.com/o/r/pull/3".into()),
        },
        Trigger::StagingSmoke {
            sha: "abc1234".into(),
            url: "https://staging.example.test".into(),
            command: "npm run smoke".into(),
            tail: "boom".into(),
        },
        Trigger::BehindMain {
            branch: "dev".into(),
            main: "main".into(),
            behind: 3,
            project: "demo".into(),
        },
    ];
    let answers = [
        json!({ "queued": true }),
        json!({ "queued": true }),
        json!({ "task": "task-12" }),
    ];
    for (trigger, answer) in triggers.iter().zip(answers) {
        let (base, seen) = fake_store(json!({ "value": answer })).await;
        let got = Client::new(base).chore_trigger(7, Some("tok"), trigger).await.unwrap();
        let args = seen.lock().unwrap()[0]["args"].clone();
        match trigger {
            Trigger::BehindMain { .. } => {
                assert_eq!(got, TriggerAnswer::Task("task-12".into()));
                assert_eq!(
                    args,
                    json!({ "trigger": "behind_main", "branch": "dev", "main": "main", "behind": 3, "project": "demo" })
                );
            }
            Trigger::PlaytestFix { .. } => {
                assert_eq!(got, TriggerAnswer::Queued);
                assert_eq!(
                    args["data"]["steps"],
                    json!(["Press jump"]),
                    "a blank step is dropped, the store refuses one"
                );
                assert_eq!(args["data"]["prUrl"], "https://github.com/o/r/pull/3");
            }
            _ => assert_eq!(args["data"]["command"], "npm run smoke"),
        }
    }
}

#[test]
fn fields_past_the_stores_limits_are_cut_so_the_report_isnt_refused() {
    let long = "é".repeat(30_000);
    let args = Trigger::RedMain {
        branch: "b".into(),
        sha: "s".into(),
        checks: (0..80).map(|i| format!("check {i}")).collect(),
        brief: long.clone(),
    }
    .args();
    super::contract::chore_trigger(&args).unwrap();
    assert_eq!(args["data"]["checks"].as_array().unwrap().len(), 50);
    let smoke = Trigger::StagingSmoke {
        sha: "s".into(),
        url: "u".into(),
        command: "c".into(),
        tail: format!("{}END", "x".repeat(30_000)),
    }
    .args();
    super::contract::chore_trigger(&smoke).unwrap();
    assert!(
        smoke["data"]["tail"].as_str().unwrap().ends_with("END"),
        "a log keeps its end"
    );
}

#[test]
fn the_contract_refuses_what_the_store_refuses() {
    assert!(super::contract::chore_trigger(&json!({ "trigger": "nope" })).is_err());
    assert!(
        super::contract::chore_trigger(&json!({ "trigger": "red_main", "data": { "sha": " ", "branch": "b" } }))
            .is_err()
    );
    assert!(
        super::contract::chore_trigger(&json!({ "trigger": "behind_main", "branch": "b", "main": "m", "behind": 0 }))
            .is_err()
    );
    assert!(super::contract::task_end(&json!({ "id": "t" })).is_err());
    assert!(super::contract::health(&json!({ "running": [{}], "paused": false, "draining": false })).is_err());
}

#[tokio::test]
async fn chore_nofix_sends_the_commit_and_branch() {
    let (base, seen) = fake_store(json!({ "value": { "sha": "abc", "branch": "dev" } })).await;
    Client::new(base)
        .chore_nofix(7, Some("tok"), "abc", "dev")
        .await
        .unwrap();
    let sent = seen.lock().unwrap()[0].clone();
    assert_eq!(sent["op"], "chore_nofix");
}

#[test]
fn a_task_tier_maps_to_the_item_flows_tier() {
    assert_eq!(Tier::of(None).item_tier(), "opus");
    assert_eq!(Tier::of(Some("smart")).item_tier(), "opus");
    assert_eq!(Tier::of(Some("fast")).item_tier(), "sonnet");
}

#[tokio::test]
async fn no_fix_sha_reads_the_marker_or_none() {
    let (base, seen) = fake_store(json!({ "value": { "noFixSha": { "sha": "abc", "branch": "dev" } } })).await;
    let got = Client::new(base).no_fix_sha(7, Some("t")).await.unwrap().unwrap();
    assert_eq!((got.sha.as_str(), got.branch.as_str()), ("abc", "dev"));
    assert_eq!(seen.lock().unwrap()[0]["op"], "chores_get");
    let (base, _) = fake_store(json!({ "value": {} })).await;
    assert!(Client::new(base).no_fix_sha(7, None).await.unwrap().is_none());
}

#[tokio::test]
async fn the_fake_store_answers_400_for_what_the_store_would_refuse() {
    let (base, seen) = fake_store(json!({ "value": {} })).await;
    let client = Client::new(base);
    let err = client.claim_task(7, Some("tok"), "  ", &agent()).await.unwrap_err();
    assert_eq!(err.code(), Some("bad_request"));
    assert!(err.is_refusal());
    let err = client.chore_nofix(7, Some("tok"), "", "dev").await.unwrap_err();
    assert_eq!(err.code(), Some("bad_request"));
    let err = client
        .task_end(7, Some("tok"), &TaskEnd::done(&"x".repeat(101), true))
        .await
        .unwrap_err();
    assert_eq!(err.code(), Some("bad_request"));
    assert_eq!(
        seen.lock().unwrap().len(),
        3,
        "each refused request was still sent and recorded"
    );
    assert!(
        client.no_fix_sha(7, Some("tok")).await.is_ok(),
        "chores_get takes no argument"
    );
}

#[test]
fn check_op_takes_each_pool_op_by_name_and_refuses_any_other() {
    assert!(super::contract::check_op("chores_get", &json!({})).is_ok());
    assert!(super::contract::check_op("chores_get", &json!("x")).is_err());
    assert!(
        super::contract::check_op("claim", &json!({ "id": "a-1" })).is_ok(),
        "agent may be left out"
    );
    assert!(super::contract::check_op("claim", &json!({ "id": "a-1", "agent": [] })).is_err());
    assert!(super::contract::check_op("claim", &json!({ "agent": {} })).is_err());
    assert!(super::contract::check_op("task_end", &json!({ "id": "t", "ok": true })).is_ok());
    assert!(super::contract::check_op("list", &json!({})).is_err());
}
