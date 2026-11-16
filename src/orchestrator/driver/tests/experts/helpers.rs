//! A session's helpers: read helpers in its lane, lane helpers in a lane of their own, results given to the session,
//! the cap, and stopping with the session.
use super::*;
use crate::expert::helpers::Asked;

fn helper(x: &Ex, id: &str, by: &str, task: &str, lane: bool) {
    x.send(
        id,
        by,
        ExpertAction::Helper {
            item: ID.into(),
            task: task.into(),
            lane,
        },
    );
}

fn notes(x: &Ex) -> Vec<String> {
    (x.st.args("report").iter())
        .filter_map(|n| n["note"].as_str().map(String::from))
        .collect()
}

async fn idle(x: &mut Ex) {
    x.start("s1", "fix it");
    x.answer("s1").await;
    x.until("idle", |x| x.detail(ID).as_deref() == Some("expert_idle"))
        .await;
}

const FOUND: &str = "Helper 1 (\"look\") finished: found look";

#[tokio::test]
async fn a_read_helper_runs_in_the_sessions_lane_and_its_result_is_told_to_an_idle_session_at_once() {
    let mut x = ex().await;
    idle(&mut x).await;
    helper(&x, "h1", "ada", "look", false);
    let a = x.answer("h1").await;
    assert_eq!(a["answer"]["helper"], "helper-1");
    x.until("the result told", |x| x.jobs().len() == 2).await;
    assert_eq!(
        x.jobs()[1].message,
        FOUND,
        "a turn of its own, with nothing of the person's"
    );
    assert_eq!(
        x.host.helper_runs.lock().unwrap()[0],
        (1, 1, false, "retaskable/expert-fix-it-fix-it".to_string())
    );
    let said = notes(&x);
    assert!(
        said.contains(&"Helper 1 started: look".to_string()) && said.contains(&FOUND.to_string()),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_result_that_comes_in_during_a_turn_waits_for_the_persons_next_message() {
    let mut x = ex().await;
    idle(&mut x).await;
    x.host.helper_hold.store(true, Ordering::SeqCst);
    helper(&x, "h1", "ada", "look", false);
    x.answer("h1").await;
    x.host.hold.store(true, Ordering::SeqCst);
    x.say("m", ID, "more", false);
    x.until("a turn running", |x| x.jobs().len() == 2).await;
    x.host.helper_go.notify_one();
    x.until("the result noted", |x| notes(x).contains(&FOUND.to_string()))
        .await;
    x.host.hold.store(false, Ordering::SeqCst);
    x.host.go.notify_one();
    x.until("idle", |x| x.detail(ID).as_deref() == Some("expert_idle"))
        .await;
    x.quiet().await;
    assert_eq!(x.jobs().len(), 2, "the result isn't told on its own");
    x.say("m2", ID, "next", false);
    x.until("the next turn", |x| x.jobs().len() == 3).await;
    assert_eq!(
        x.jobs()[2].message,
        format!("{FOUND}\n\nnext"),
        "before the person's words"
    );
    x.say("m3", ID, "after", false);
    x.until("another", |x| x.jobs().len() == 4).await;
    assert_eq!(x.jobs()[3].message, "after", "given once");
}

#[tokio::test]
async fn a_helper_that_fails_says_why_and_two_run_at_most() {
    let mut x = ex().await;
    idle(&mut x).await;
    x.host.helper_hold.store(true, Ordering::SeqCst);
    *x.host.helper_fails.lock().unwrap() = Some("it crashed".into());
    for (i, task) in ["a", "b", "c"].iter().enumerate() {
        helper(&x, &format!("h{i}"), "ada", task, false);
    }
    let third = x.answer("h2").await;
    assert_eq!(
        third["reason"],
        "2 helpers are running for this session already: wait for one to finish."
    );
    assert_eq!(x.answer("h0").await["answer"]["helper"], "helper-1");
    x.host.helper_go.notify_one();
    x.host.helper_go.notify_one();
    x.until("a failure noted", |x| {
        notes(x)
            .iter()
            .any(|n| n.starts_with("Helper 1 (\"a\") didn't finish: it crashed"))
    })
    .await;
    helper(&x, "h4", "ada", "d", false);
    assert_eq!(
        x.answer("h4").await["answer"]["helper"],
        "helper-3",
        "numbered in the order started"
    );
}

#[tokio::test]
async fn only_the_starter_may_ask_for_one_and_a_parked_session_has_no_lane_for_it() {
    let mut x = ex().await;
    idle(&mut x).await;
    helper(&x, "h1", "grace", "look", false);
    let a = x.answer("h1").await;
    assert!(a["reason"].as_str().unwrap().starts_with("Only ada"), "{a}");
    x.end("e1", ID, "park");
    x.answer("e1").await;
    helper(&x, "h2", "ada", "look", false);
    let a = x.answer("h2").await;
    assert_eq!(
        a["reason"],
        "This session is parked: a helper needs its lane. Send a message first."
    );
    assert!(x.host.helper_runs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_lane_helper_takes_a_free_lane_on_its_own_branch_and_gives_it_back_naming_the_branch() {
    let mut x = ex().await;
    idle(&mut x).await;
    helper(&x, "h1", "ada", "edit it", true);
    x.answer("h1").await;
    x.until("told", |x| x.jobs().len() == 2).await;
    assert_eq!(
        x.host.helper_runs.lock().unwrap()[0],
        (1, 2, true, "retaskable/expert-fix-it-fix-it".to_string())
    );
    let branch = "retaskable/expert-fix-it-fix-it-h1";
    let said = notes(&x);
    assert!(said.contains(&format!("Helper 1 is in lane 2 on {branch}")), "{said:?}");
    let end = format!(
        "Helper 1 (\"edit it\") finished: found edit it\n\nIts commits are on {branch}, from your last commit: merge it in if you want them."
    );
    assert_eq!(x.jobs()[1].message, end);
    assert_eq!(x.rig.machine.pool.holder(2), None, "its lane is free again");
}

#[tokio::test]
async fn a_lane_helper_waits_for_a_free_lane_and_stops_with_the_session() {
    let mut x = ex_lanes(1).await;
    idle(&mut x).await;
    helper(&x, "h1", "ada", "edit it", true);
    x.answer("h1").await;
    x.quiet().await;
    let said = notes(&x);
    assert_eq!(
        said.iter()
            .filter(|n| n.as_str() == "Helper 1 is waiting for a free lane")
            .count(),
        1,
        "{said:?}"
    );
    assert!(x.host.helper_runs.lock().unwrap().is_empty());
    x.end("e1", ID, "park");
    x.answer("e1").await;
    let said = notes(&x);
    assert!(
        said.contains(
            &"Helper 1 (\"edit it\") was stopped: anything it committed is on retaskable/expert-fix-it-fix-it-h1"
                .to_string()
        ),
        "{said:?}"
    );
    x.quiet().await;
    assert!(
        x.host.helper_runs.lock().unwrap().is_empty(),
        "it never took the freed lane"
    );
    assert_eq!(x.jobs().len(), 1, "nothing told to a parked session");
}

#[tokio::test]
async fn a_running_helper_stops_when_the_session_is_parked_and_its_result_goes_nowhere() {
    let mut x = ex().await;
    idle(&mut x).await;
    x.host.helper_hold.store(true, Ordering::SeqCst);
    helper(&x, "h1", "ada", "look", false);
    x.answer("h1").await;
    x.until("started", |x| x.host.helper_runs.lock().unwrap().len() == 1)
        .await;
    x.end("e1", ID, "park");
    x.answer("e1").await;
    assert!(notes(&x).contains(&"Helper 1 (\"look\") was stopped".to_string()));
    assert_eq!(x.jobs().len(), 1);
}

#[tokio::test]
async fn a_stop_leaves_the_helpers_running() {
    let mut x = ex().await;
    idle(&mut x).await;
    x.host.helper_hold.store(true, Ordering::SeqCst);
    helper(&x, "h1", "ada", "look", false);
    x.answer("h1").await;
    x.send("st", "ada", ExpertAction::Stop { item: ID.into() });
    x.answer("st").await;
    x.quiet().await;
    assert!(!notes(&x).iter().any(|n| n.contains("was stopped")), "{:?}", notes(&x));
    x.host.helper_go.notify_one();
    x.until("it finished", |x| notes(x).contains(&FOUND.to_string())).await;
}

#[tokio::test]
async fn the_helpers_a_turn_asks_for_start_after_its_summary_and_one_refused_says_why() {
    let mut x = ex().await;
    x.host.helper_hold.store(true, Ordering::SeqCst);
    *x.host.asks.lock().unwrap() = ["one", "two", "three"]
        .iter()
        .map(|t| Asked {
            task: (*t).into(),
            lane: *t == "two",
        })
        .collect();
    x.start("s1", "fix it");
    x.until("the third refused", |x| {
        notes(x).contains(
            &"A helper wasn't started: 2 helpers are running for this session already: wait for one to finish."
                .to_string(),
        )
    })
    .await;
    let said = notes(&x);
    let at = |t: &str| {
        said.iter()
            .position(|n| n.starts_with(t))
            .unwrap_or_else(|| panic!("{t}: {said:?}"))
    };
    assert!(at("Did: fix it") < at("Helper 1 started: one"), "{said:?}");
    assert!(at("Helper 1 started: one") < at("Helper 2 started: two"));
    x.until("the lane helper in a lane", |x| {
        x.host.helper_runs.lock().unwrap().len() == 2
    })
    .await;
    let runs = x.host.helper_runs.lock().unwrap().clone();
    assert_eq!((runs[0].0, runs[0].2, runs[1].0, runs[1].2), (1, false, 2, true));
}
