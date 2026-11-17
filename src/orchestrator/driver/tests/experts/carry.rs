//! A session that continues a Claude Code session (`expert_start { continue }`): its branch, its note in the store,
//! its first turn's handover, and the one open session a Claude Code session has.
use super::*;
use crate::expert::claude_sessions::Continued;

const SESSION: &str = "7f3a-session";

fn claude(x: &Ex, branch: Option<&str>) {
    x.host.claude.lock().unwrap().insert(
        SESSION.into(),
        Continued {
            session_id: SESSION.into(),
            file: "unused".into(),
            title: "Fix the login".into(),
            folder: "/work/login".into(),
            branch: branch.map(String::from),
        },
    );
}

fn start_from(x: &Ex, id: &str, request: &str, session: &str) {
    x.send(
        id,
        "ada",
        ExpertAction::Start {
            request: request.into(),
            continue_from: Some(session.into()),
        },
    );
}

/// The notes the item's history got, in order.
fn notes(x: &Ex) -> Vec<String> {
    (x.st.args("report").iter())
        .filter_map(|n| n["note"].as_str().map(String::from))
        .collect()
}

#[tokio::test]
async fn a_continued_session_takes_the_claude_code_sessions_title_branch_and_handover() {
    let mut x = ex().await;
    claude(&x, Some("feature/login"));
    start_from(&x, "s1", "carry on", SESSION);
    let a = x.answer("s1").await;
    // Titled after the session, not the request.
    assert_eq!(a["answer"]["itemId"], "expert-fix-the-login", "{a}");
    x.until("the first turn", |x| {
        x.detail("expert-fix-the-login").as_deref() == Some("expert_idle")
    })
    .await;
    let add = &x.st.args("add")[0];
    assert_eq!(add["items"][0]["title"], "Fix the login");
    assert_eq!(add["items"][0]["remaining"], "carry on");
    // Its branch started at the tip of the folder's, so the lane picks it up as a parked session's.
    assert_eq!(
        x.host.carried.lock().unwrap()[..],
        [(
            "retaskable/expert-fix-the-login-fix-the-login".to_string(),
            "/work/login".to_string()
        )]
    );
    let j = &x.jobs()[0];
    assert_eq!((j.prepare, j.from_branch, j.session_id.as_deref()), (true, true, None));
    assert_eq!(j.message, format!("HANDOVER of {SESSION}\ncarry on"));
    assert!(
        notes(&x).contains(&"Carries on from feature/login in /work/login".to_string()),
        "{:?}",
        notes(&x)
    );
    // The note in the store is an id and a title, no path.
    let set = &x.st.args("expert_continues_set")[0];
    assert_eq!(
        set["from"],
        json!({ "sessionId": SESSION, "title": "Fix the login", "branch": "feature/login" })
    );
    assert!(!set.to_string().contains("/work"), "{set}");
}

#[tokio::test]
async fn the_handover_is_for_the_first_answered_turn_only() {
    let mut x = ex().await;
    claude(&x, Some("feature/login"));
    start_from(&x, "s1", "carry on", SESSION);
    x.answer("s1").await;
    x.until("the first turn", |x| {
        x.jobs().len() == 1 && x.detail("expert-fix-the-login").as_deref() == Some("expert_idle")
    })
    .await;
    x.say("m1", "expert-fix-the-login", "and the tests", false);
    x.until("the second turn", |x| x.jobs().len() == 2).await;
    assert_eq!(x.jobs()[1].message, "and the tests");
}

#[tokio::test]
async fn a_folder_that_isnt_the_projects_starts_from_the_base_and_says_why() {
    let mut x = ex().await;
    claude(&x, Some("main"));
    *x.host.carry.lock().unwrap() = Some("it worked on main itself".into());
    start_from(&x, "s1", "carry on", SESSION);
    x.answer("s1").await;
    x.until("the first turn", |x| x.jobs().len() == 1).await;
    let j = &x.jobs()[0];
    assert!(!j.from_branch);
    assert!(j.message.starts_with("HANDOVER"), "the conversation still carries over");
    let said = notes(&x);
    assert!(
        said.iter()
            .any(|n| n.starts_with("Starts from ") && n.ends_with(": it worked on main itself")),
        "{said:?}"
    );
}

#[tokio::test]
async fn a_claude_code_session_that_is_gone_is_refused_and_nothing_is_added() {
    let mut x = ex().await;
    start_from(&x, "s1", "carry on", "no-such-session");
    let a = x.answer("s1").await;
    assert_eq!(
        (a["type"].as_str(), a["reason"].as_str()),
        (
            Some("nack"),
            Some("That Claude Code session isn't on this computer any more.")
        )
    );
    assert_eq!(x.st.count("add"), 0);
}

#[tokio::test]
async fn continuing_the_same_claude_code_session_again_messages_the_open_session() {
    let mut x = ex().await;
    claude(&x, None);
    start_from(&x, "s1", "carry on", SESSION);
    x.answer("s1").await;
    x.until("the first turn", |x| {
        x.jobs().len() == 1 && x.detail("expert-fix-the-login").as_deref() == Some("expert_idle")
    })
    .await;
    start_from(&x, "s2", "also this", SESSION);
    let a = x.answer("s2").await;
    assert_eq!(a["answer"]["itemId"], "expert-fix-the-login");
    x.until("the message run", |x| x.jobs().len() == 2).await;
    assert_eq!(x.jobs()[1].message, "also this");
    assert_eq!(x.st.count("add"), 1, "no second session");
}

#[tokio::test]
async fn a_session_whose_file_has_gone_runs_the_persons_message_alone() {
    let mut x = ex().await;
    claude(&x, None);
    x.host.file_gone.store(true, Ordering::SeqCst);
    start_from(&x, "s1", "carry on", SESSION);
    x.answer("s1").await;
    x.until("the first turn", |x| x.jobs().len() == 1).await;
    assert_eq!(x.jobs()[0].message, "carry on");
}
