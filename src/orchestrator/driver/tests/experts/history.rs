//! `session_history`: a session's past turns, with the running one left to `watch` and a continued session's Claude
//! Code conversation shown to the person who started it only.
use super::*;
use crate::expert::claude_sessions::Continued;

fn history(x: &Ex, id: &str, by: &str, item: &str) {
    x.send(
        id,
        by,
        ExpertAction::History {
            item: item.into(),
            before: None,
            limit: None,
        },
    );
}

fn at_of(a: &Value) -> Vec<i64> {
    (a["answer"]["turns"].as_array().unwrap().iter())
        .map(|t| t["at"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn an_idle_session_answers_with_its_runs_turns_any_member_may_ask() {
    let mut x = ex().await;
    x.start("s1", "fix it");
    x.answer("s1").await;
    x.until("idle", |x| x.detail(ID).as_deref() == Some("expert_idle"))
        .await;
    history(&x, "h1", "grace", ID);
    let a = x.answer("h1").await;
    assert_eq!(a["type"], "ack");
    assert_eq!(at_of(&a), [100, 200]);
    assert_eq!(a["answer"]["more"], false);
}

#[tokio::test]
async fn the_running_turn_is_left_to_watch() {
    let mut x = ex().await;
    x.host.hold.store(true, Ordering::SeqCst);
    x.start("s1", "fix it");
    x.answer("s1").await;
    x.until("running", |x| x.jobs().len() == 1).await;
    history(&x, "h1", "ada", ID);
    let a = x.answer("h1").await;
    assert!(
        at_of(&a).is_empty(),
        "no run file yet, and the running turn would be dropped anyway: {a}"
    );
    x.host.go.notify_one();
    x.until("idle", |x| x.detail(ID).as_deref() == Some("expert_idle"))
        .await;
    x.say("m", ID, "more", false);
    x.host.hold.store(true, Ordering::SeqCst);
    x.until("second turn", |x| x.jobs().len() == 2).await;
    history(&x, "h2", "ada", ID);
    let a = x.answer("h2").await;
    assert_eq!(at_of(&a), [100], "the running turn (the last meta) is dropped");
}

#[tokio::test]
async fn a_continued_conversation_is_its_starters_alone() {
    let mut x = ex().await;
    x.host.claude.lock().unwrap().insert(
        "7f3a-session".into(),
        Continued {
            session_id: "7f3a-session".into(),
            file: "unused".into(),
            title: "Fix the login".into(),
            folder: "/work/login".into(),
            branch: None,
        },
    );
    x.send(
        "s1",
        "ada",
        ExpertAction::Start {
            request: "carry on".into(),
            continue_from: Some("7f3a-session".into()),
        },
    );
    x.answer("s1").await;
    let id = "expert-fix-the-login";
    x.until("idle", |x| x.detail(id).as_deref() == Some("expert_idle"))
        .await;
    history(&x, "h1", "ada", id);
    assert_eq!(at_of(&x.answer("h1").await), [10, 100, 200]);
    history(&x, "h2", "grace", id);
    assert_eq!(
        at_of(&x.answer("h2").await),
        [100, 200],
        "the conversation is its starter's"
    );
}

#[tokio::test]
async fn an_unknown_item_is_not_an_expert_session() {
    let mut x = ex().await;
    history(&x, "h1", "ada", "nope");
    let a = x.answer("h1").await;
    assert_eq!(
        (a["type"].as_str(), a["reason"].as_str()),
        (Some("nack"), Some("That's not an expert session."))
    );
}
