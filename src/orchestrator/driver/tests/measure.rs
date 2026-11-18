//! The measurement events written at claim mode's points (agent-telemetry.md section 3.4): every role's `session_end`
//! with cost and usage, an implementer's settle carrying its sessions' sum, `needs_person` codes, the way an item merged
//! and how a chore ended. Runs mode's are in `runs/measure.rs`, and the plan's and the chore's in their own flows' tests.
use super::settling::ended;
use super::*;
use crate::engines::EngineId;
use crate::engines::choose::Role;
use crate::engines::engine::{SessionResult as EngineResult, Tokens};
use crate::orchestrator::live::sessions::{SessionInfo, record_session_end};

fn info(role: Role, item: &str, lane: Option<u32>, run: Option<&str>) -> SessionInfo {
    SessionInfo {
        project: "p0".into(),
        item_id: item.into(),
        title: None,
        role,
        lane,
        engine: EngineId::Claude,
        model: "sonnet".into(),
        run: run.map(String::from),
    }
}

fn engine_result(cost: Option<f64>) -> EngineResult {
    EngineResult {
        ok: true,
        subtype: "success".into(),
        cost_usd: cost,
        session_id: "sess-1".into(),
        tokens: Some(Tokens {
            input: 100,
            output: 20,
            cache: 300,
        }),
        ..Default::default()
    }
}

fn logged(rig: &mut Rig, kind: &str) -> Vec<Value> {
    (rig.lp(0).events().recent().iter())
        .map(Entry::to_value)
        .filter(|e| e["kind"] == kind)
        .collect()
}

#[tokio::test]
async fn every_role_writes_a_session_end_with_its_cost_and_usage() {
    let (base, _asked) = stub().await;
    let mut rig = rig(2, &[&base]);
    let roles = [
        Role::Reviewer,
        Role::Drafter,
        Role::Planner,
        Role::Scout,
        Role::Assistant,
        Role::Writer,
        Role::Triager,
        Role::Implementer,
    ];
    for role in roles {
        let result = engine_result(Some(0.25));
        let sessions = rig.live.sessions().clone();
        let run = (role == Role::Planner).then_some("abc123");
        sessions
            .track(info(role, "thing", None, run), async {
                record_session_end(EngineId::Claude, &result)
            })
            .await;
    }
    rig.machine.pass().await;
    let ends = logged(&mut rig, "session_end");
    assert_eq!(ends.len(), roles.len(), "{ends:?}");
    for (end, role) in ends.iter().zip(roles) {
        assert_eq!(end["role"], role.name());
        assert_eq!(
            (end["engine"].as_str(), end["model"].as_str()),
            (Some("claude"), Some("sonnet"))
        );
        assert_eq!(
            (end["cost"].as_f64(), end["ok"].as_bool(), end["item"].as_str()),
            (Some(0.25), Some(true), Some("thing"))
        );
        assert_eq!(
            (
                end["inputTokens"].as_u64(),
                end["outputTokens"].as_u64(),
                end["cacheTokens"].as_u64()
            ),
            (Some(100), Some(20), Some(300))
        );
        assert_eq!(end["tokens"], 120);
        assert_eq!(end["session"], "sess-1");
    }
    // A planner's session says which run and piece it was for; the others are not run work.
    let planner = ends.iter().find(|e| e["role"] == "planner").unwrap();
    assert_eq!(
        (planner["run"].as_str(), planner["piece"].as_str()),
        (Some("abc123"), Some("thing"))
    );
    assert!(
        ends.iter()
            .filter(|e| e["role"] != "planner")
            .all(|e| e.get("run").is_none())
    );
    // Written once: another pass adds nothing.
    rig.machine.pass().await;
    assert_eq!(logged(&mut rig, "session_end").len(), roles.len());
}

#[tokio::test]
async fn a_session_with_no_reported_cost_leaves_it_out_and_a_failed_one_says_why() {
    let (base, _asked) = stub().await;
    let mut rig = rig(2, &[&base]);
    let mut result = engine_result(None);
    result.ok = false;
    result.error = Some("it fell over ".repeat(100));
    result.tokens = None;
    let sessions = rig.live.sessions().clone();
    sessions
        .track(info(Role::Reviewer, "x", None, None), async {
            record_session_end(EngineId::Cursor, &result)
        })
        .await;
    rig.machine.pass().await;
    let end = &logged(&mut rig, "session_end")[0];
    assert!(end.get("cost").is_none() && end.get("tokens").is_none());
    assert_eq!(
        (end["ok"].as_bool(), end["engine"].as_str()),
        (Some(false), Some("cursor"))
    );
    assert!(end["error"].as_str().unwrap().chars().count() <= crate::telemetry::MAX_TEXT);
}

#[tokio::test]
async fn a_claimed_implementers_settle_carries_the_cost_of_its_sessions() {
    let (mut rig, _fake, _asked) = ended(SessionResult::Interrupted, &Default::default()).await;
    let sessions = rig.live.sessions().clone();
    for cost in [0.5, 0.25] {
        let result = engine_result(Some(cost));
        sessions
            .track(info(Role::Implementer, "a", Some(1), None), async {
                record_session_end(EngineId::Claude, &result)
            })
            .await;
    }
    rig.machine.pass().await;
    let ends = logged(&mut rig, "session_end");
    // One `session_end` for the settle, not one per engine session.
    assert_eq!(ends.len(), 1, "{ends:?}");
    assert_eq!(
        (ends[0]["cost"].as_f64(), ends[0]["tokens"].as_u64()),
        (Some(0.75), Some(240))
    );
    assert_eq!(
        (ends[0]["role"].as_str(), ends[0]["subtype"].as_str()),
        (Some("implementer"), Some("interrupted"))
    );
    assert_eq!(ends[0]["model"], "sonnet");
}

#[tokio::test]
async fn a_session_that_needs_a_human_writes_needs_person_with_a_code_and_the_words() {
    let why = "The work is committed but couldn't be published: git push rejected (non-fast-forward)";
    let (mut rig, _fake, _asked) = ended(SessionResult::NeedsHuman(why.into()), &Default::default()).await;
    rig.machine.pass().await;
    let needs = logged(&mut rig, "needs_person");
    assert_eq!(needs.len(), 1, "{needs:?}");
    assert_eq!(
        (
            needs[0]["item"].as_str(),
            needs[0]["reason"].as_str(),
            needs[0]["from"].as_str()
        ),
        (Some("a"), Some("push_rejected"), Some("implementing"))
    );
    assert_eq!(needs[0]["text"], why);
}

fn put_chore(rig: &mut Rig, id: &str, state: ItemState) {
    let work = serde_json::from_value(json!({ "kind": "tidy", "title": "Tidy" })).unwrap();
    rig.lp(0).take_chore(id, work, state);
}

#[tokio::test]
async fn a_chore_that_found_nothing_left_ends_nothing_left_and_one_that_merged_ends_merged() {
    let (base, _asked) = stub().await;
    let mut rig = rig(2, &[&base]);
    put_chore(&mut rig, "task-1", ItemState::Done);
    put_chore(&mut rig, "task-2", ItemState::Done);
    rig.lp(0).measure_nothing_left("task-1");
    rig.lp(0).forget_chore("task-1");
    rig.lp(0).forget_chore("task-2");
    let ends = logged(&mut rig, "chore_end");
    let outcome = |id: &str| ends.iter().find(|e| e["item"] == id).unwrap()["outcome"].clone();
    assert_eq!(
        (outcome("task-1"), outcome("task-2")),
        (json!("nothing_left"), json!("merged"))
    );
    assert_eq!(logged(&mut rig, "chore_start").len(), 2);
}

#[tokio::test]
async fn a_change_found_merged_when_the_agent_came_back_says_away_and_a_hands_off_says_auto_merge() {
    let (base, _asked) = stub().await;
    let mut rig = rig(2, &[&base]);
    for (id, land) in [("a", "person"), ("b", "auto_merge"), ("c", "person")] {
        let mut record = crate::store::RunRecord::default();
        record.flow.pr_number = Some(7);
        record.flow.land = Some(land.into());
        rig.lp(0).records.insert(id.into(), record);
    }
    rig.lp(0).measure_away("c");
    for id in ["a", "b", "c"] {
        rig.lp(0).measure_item_merged(id, Some("deadbeef"));
    }
    let merged = logged(&mut rig, "merged");
    let how = |id: &str| merged.iter().find(|e| e["item"] == id).unwrap()["how"].clone();
    assert_eq!(
        (how("a"), how("b"), how("c")),
        (json!("merge"), json!("auto_merge"), json!("away"))
    );
    assert!(merged.iter().all(|e| e["pr"] == 7 && e["commit"] == "deadbeef"));
}
