//! The draft step (`driver/drafts.rs`): a fake store and a fake host whose drafter answers are scripted.
use super::*;
use crate::drafts::DrafterResult;
use crate::implementer::publish::Published;
use std::path::{Path, PathBuf};

use crate::config::ProjectConfig;
use crate::orchestrator::driver::{DraftHost, DraftJob};

/// What the fake host was asked, and what it answers.
#[derive(Default)]
pub(super) struct Host {
    answers: Mutex<Vec<Value>>,
    pub(super) jobs: Mutex<Vec<(String, bool, String)>>,
    engines: Mutex<Vec<Option<crate::engines::EngineId>>>,
    published: Mutex<Vec<(String, String)>>,
    hold: AtomicBool,
    merges: Mutex<u32>,
    /// What the classifier answers: a confident bug when set.
    bug: AtomicBool,
    requests: Mutex<Vec<Option<String>>>,
}

impl DraftHost for Host {
    fn prepare(&self, _: &ProjectConfig, id: &str, _: &str) -> Result<PathBuf> {
        Ok(PathBuf::from(format!("draft-{id}")))
    }
    fn run(&self, job: DraftJob) -> BoxFuture<'static, Result<DrafterResult>> {
        self.jobs
            .lock()
            .unwrap()
            .push((job.session_id.clone(), job.resume, job.follow_up.clone()));
        self.engines.lock().unwrap().push(job.engine);
        self.requests.lock().unwrap().push(job.draft.request.clone());
        let answer = self.answers.lock().unwrap().remove(0);
        let hold = self.hold.load(Ordering::SeqCst);
        async move {
            if hold {
                std::future::pending::<()>().await;
            }
            Ok(serde_json::from_value(answer)?)
        }
        .boxed()
    }
    fn publish(&self, _: &ProjectConfig, _: &Path, _: &str, title: &str, body: &str) -> Result<Published> {
        self.published.lock().unwrap().push((title.into(), body.into()));
        Ok(Published {
            url: "https://github.com/o/r/pull/9".into(),
            number: 9,
            created: true,
        })
    }
    fn remove(&self, _: &ProjectConfig, _: &str) {}
    fn merge(&self, _: &ProjectConfig, _: u64, _: bool) -> Result<()> {
        *self.merges.lock().unwrap() += 1;
        Ok(())
    }
    fn close(&self, _: &ProjectConfig, _: u64) -> Result<()> {
        Ok(())
    }
    fn rebase(&self, _: &ProjectConfig, _: &str, _: &str) -> Result<bool> {
        Ok(true)
    }
    fn push(&self, _: &ProjectConfig, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn fetch(&self, _: &ProjectConfig) -> Result<()> {
        Ok(())
    }
    fn classify(&self, _: &ProjectConfig, _: String) -> BoxFuture<'static, crate::ask::classify::Classified> {
        use crate::ask::classify::{Classified, Kind, Sorted};
        let kind = if self.bug.load(Ordering::SeqCst) {
            Kind::Bug
        } else {
            Kind::Idea
        };
        async move {
            Classified::Sorted(Sorted {
                kind,
                confidence: 0.9,
                title: None,
            })
        }
        .boxed()
    }
}

/// A store whose `draft_claim_next` hands out `draft` once; every op is recorded.
/// The op `fail` is refused with a 500.
async fn store_failing(draft: Value, fail: &'static str) -> (String, Asked) {
    let asked: Asked = Default::default();
    let (seen, handed) = (asked.clone(), Arc::new(AtomicBool::new(false)));
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, handed, draft) = (seen.clone(), handed.clone(), draft.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                if op == fail {
                    return (
                        axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                        Json(json!({ "error": "boom" })),
                    );
                }
                let value = match op.as_str() {
                    "list" => json!([]),
                    "ideas_list" => json!({ "round": null, "ideas": [{
                        "id": "i1", "title": "Save is dead", "state": "waiting", "at": 0, "planId": "d1",
                        "idea": { "experience": "Nothing happens" }, "addedBy": { "login": "ann", "at": 1 } }] }),
                    "draft_take" => json!({ "token": "lease-t", "draft": {
                        "id": "d1", "status": "needs_person", "detail": "awaiting_approval",
                        "prUrl": "https://github.com/o/r/pull/9", "branch": "retaskable/spec-d1-charts",
                        "requestedBy": "ada" } }),
                    "draft_claim_next" if !handed.swap(true, Ordering::SeqCst) => {
                        json!({ "draft": draft, "token": "lease-d" })
                    }
                    _ => Value::Null,
                };
                (axum::http::StatusCode::OK, Json(json!({ "value": value })))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

fn claimed() -> Value {
    json!({ "id": "d1", "request": "Add charts", "titles": ["Charts"] })
}

async fn rig_with(draft: Value, answers: Vec<Value>) -> (Rig, Arc<Host>, Asked) {
    rig_failing(draft, answers, "").await
}

async fn rig_failing(draft: Value, answers: Vec<Value>, fail: &'static str) -> (Rig, Arc<Host>, Asked) {
    let (base, asked) = store_failing(draft, fail).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
    rig.lp(0).flags = Arc::new(crate::flags::Flags::fixed(&["watchdog-anomaly-rules"]));
    let host = Arc::new(Host {
        answers: Mutex::new(answers),
        ..Default::default()
    });
    rig.machine.drafts = Some(host.clone());
    (rig, host, asked)
}

/// Passes until the draft task has sent `n` reports.
async fn reports(rig: &mut Rig, asked: &Asked, n: usize) -> Vec<Value> {
    for _ in 0..200 {
        rig.machine.pass().await;
        let sent: Vec<Value> = (asked.lock().unwrap().iter())
            .filter(|(op, _)| op == "draft_report")
            .map(|(_, a)| a.clone())
            .collect();
        if sent.len() >= n {
            return sent;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the draft never reported {n} times");
}

fn claims(asked: &Asked) -> usize {
    ops(asked).iter().filter(|o| *o == "draft_claim_next").count()
}

#[tokio::test]
async fn a_claimed_draft_runs_and_its_pr_is_reported_with_the_link() {
    let answer = json!({"status": "pr_opened", "changeTitle": "Spec: Charts", "changeBody": "Adds charts.",
        "itemIds": ["charts-1", "charts-2"], "summary": "Charts",
        "pieces": [{"title": "Charts", "kind": "feature"}],
        "breakdown": {"build": [{"id": "charts-1", "line": "Draw them", "size": "M"}], "people": []}});
    let (mut rig, host, asked) = rig_with(claimed(), vec![answer]).await;
    let sent = reports(&mut rig, &asked, 2).await;
    assert_eq!(sent[0]["status"], "drafting");
    assert!(sent[0]["sessionId"].as_str().is_some_and(|s| !s.is_empty()));
    let done = &sent[1];
    assert_eq!(
        (done["id"].as_str(), done["token"].as_str()),
        (Some("d1"), Some("lease-d"))
    );
    assert_eq!(done["status"], "needs_person");
    assert_eq!(done["detail"], "awaiting_approval");
    assert_eq!(done["prUrl"], "https://github.com/o/r/pull/9");
    assert_eq!(done["itemIds"], json!(["charts-1", "charts-2"]));
    assert_eq!(done["titles"], json!(["Charts"]));
    assert_eq!(done["breakdown"]["build"][0]["size"], "M");
    assert_eq!(host.published.lock().unwrap()[0].0, "Spec: Charts");
    assert!(!ops(&asked).contains(&"add".to_string()));
}

#[tokio::test]
async fn a_revision_that_names_no_items_leaves_the_earlier_ones() {
    let answer = json!({"status": "pr_opened", "changeTitle": "Spec: Charts", "summary": "Fixed",
        "pieces": [{"title": "Charts", "kind": "tweak"}]});
    let (mut rig, _host, asked) = rig_with(claimed(), vec![answer]).await;
    let sent = reports(&mut rig, &asked, 2).await;
    assert!(sent[1].get("itemIds").is_none());
}

#[tokio::test]
async fn a_second_draft_isnt_claimed_while_one_is_running() {
    let (mut rig, host, asked) = rig_with(claimed(), vec![json!({"status": "failed", "summary": "x"})]).await;
    host.hold.store(true, Ordering::SeqCst);
    reports(&mut rig, &asked, 1).await;
    for _ in 0..5 {
        rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(claims(&asked), 1);
    assert_eq!(rig.lp(0).draft.as_ref().map(|d| d.id.as_str()), Some("d1"));
    assert_eq!(rig.live.snapshot().projects["p0"].drafting.as_deref(), Some("d1"));
}

#[tokio::test]
async fn the_next_draft_is_claimed_once_the_one_ahead_has_ended() {
    let (mut rig, _host, asked) = rig_with(claimed(), vec![json!({"status": "failed", "summary": "x"})]).await;
    reports(&mut rig, &asked, 2).await;
    for _ in 0..50 {
        rig.machine.pass().await;
        if claims(&asked) >= 2 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no second claim");
}

#[tokio::test]
async fn questions_are_reported_for_a_person_to_answer() {
    let answer = json!({"status": "needs_clarification", "summary": "Too vague",
        "questions": [{"header": "Who", "question": "Who sees the charts?", "options": ["Everyone", "Admins"]}]});
    let (mut rig, host, asked) = rig_with(claimed(), vec![answer]).await;
    let sent = reports(&mut rig, &asked, 2).await;
    assert_eq!(sent[1]["status"], "needs_person");
    assert_eq!(sent[1]["detail"], "needs_clarification");
    assert_eq!(sent[1]["questions"], json!(["Who sees the charts?"]));
    assert!(host.published.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_failed_draft_is_reported_failed() {
    let retried = json!({ "id": "d1", "request": "Add charts", "retried": true });
    let (mut rig, host, asked) = rig_with(retried, vec![json!({"status": "failed", "summary": "No luck"})]).await;
    let sent = reports(&mut rig, &asked, 2).await;
    assert_eq!(sent[1]["detail"], "failed");
    assert_eq!(sent[1]["summary"], "No luck");
    assert!(host.published.lock().unwrap().is_empty());
}

#[tokio::test]
async fn all_fixes_are_added_as_items_and_open_no_pr() {
    let answer = json!({"status": "pr_opened", "summary": "Two fixes",
        "fixes": [{"id": "fix-1", "title": "Button is blue", "remaining": "Make it green"}],
        "pieces": [{"title": "Button is blue", "kind": "fix"}]});
    let (mut rig, host, asked) = rig_with(claimed(), vec![answer]).await;
    let sent = reports(&mut rig, &asked, 2).await;
    let add = asked
        .lock()
        .unwrap()
        .iter()
        .find(|(op, _)| op == "add")
        .cloned()
        .unwrap()
        .1;
    assert_eq!(add["via"], "chat");
    assert_eq!(add["items"][0]["id"], "fix-1");
    assert_eq!(add["items"][0]["title"], "Fix: Button is blue");
    assert_eq!(sent[1]["status"], "merged");
    assert!(host.published.lock().unwrap().is_empty(), "no pull request");
}

#[tokio::test]
async fn a_restart_resumes_the_session_that_was_drafting() {
    let draft = json!({"id": "d1", "request": "Add charts", "titles": [], "sessionId": "sess-9", "branch": "retaskable/spec-d1-x"});
    let (mut rig, host, asked) = rig_with(draft, vec![json!({"status": "failed", "summary": "x"})]).await;
    let sent = reports(&mut rig, &asked, 2).await;
    let jobs = host.jobs.lock().unwrap().clone();
    assert_eq!(
        jobs,
        vec![("sess-9".into(), true, "You were interrupted; continue.".into())]
    );
    assert_eq!(sent[0]["sessionId"], "sess-9");
    assert_eq!(sent[0]["branch"], "retaskable/spec-d1-x");
}

#[tokio::test]
async fn nothing_is_claimed_while_paused() {
    let (mut rig, host, asked) = rig_with(claimed(), vec![]).await;
    rig.pause("p0", true);
    for _ in 0..3 {
        rig.machine.pass().await;
    }
    assert_eq!(claims(&asked), 0);
    assert!(host.jobs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_draft_takes_a_session_slot() {
    let (mut rig, host, asked) = rig_with(claimed(), vec![json!({"status": "failed", "summary": "x"})]).await;
    rig.machine.concurrency = 1;
    host.hold.store(true, Ordering::SeqCst);
    reports(&mut rig, &asked, 1).await;
    rig.machine.pass().await;
    assert!(
        !ops(&asked).contains(&"claim_next".to_string()),
        "the only slot is the draft's, so no item is claimed"
    );
}

#[tokio::test]
async fn send_back_notes_resume_the_session_and_the_pr_is_reported_again() {
    let draft = json!({"id": "d1", "request": "Add charts", "titles": ["Charts"], "sessionId": "sess-9",
        "branch": "retaskable/spec-d1-x", "notes": "Shorter, please."});
    let answer = json!({"status": "pr_opened", "changeTitle": "Spec: Charts", "summary": "Shorter"});
    let (mut rig, host, asked) = rig_with(draft, vec![answer]).await;
    let sent = reports(&mut rig, &asked, 2).await;
    let jobs = host.jobs.lock().unwrap().clone();
    assert_eq!(jobs.len(), 1);
    assert_eq!((jobs[0].0.as_str(), jobs[0].1), ("sess-9", true));
    assert!(jobs[0].2.contains("Shorter, please."), "{}", jobs[0].2);
    assert_eq!(sent[1]["detail"], "awaiting_approval");
    assert_eq!(sent[1]["prUrl"], "https://github.com/o/r/pull/9");
    assert!(!ops(&asked).contains(&"draft_answer_taken".to_string()));
}

#[tokio::test]
async fn answers_resume_the_session_and_are_marked_taken() {
    let draft = json!({"id": "d1", "request": "Add charts", "titles": [], "sessionId": "sess-9",
        "answer": {"answers": ["Admins", "Monthly"]}});
    let answer = json!({"status": "pr_opened", "changeTitle": "Spec: Charts", "summary": "Done"});
    let (mut rig, host, asked) = rig_with(draft, vec![answer]).await;
    let sent = reports(&mut rig, &asked, 2).await;
    let jobs = host.jobs.lock().unwrap().clone();
    assert_eq!((jobs[0].0.as_str(), jobs[0].1), ("sess-9", true));
    assert!(jobs[0].2.contains("Admins\nMonthly"), "{}", jobs[0].2);
    assert_eq!(sent[1]["detail"], "awaiting_approval");
    let taken: Vec<Value> = asked
        .lock()
        .unwrap()
        .iter()
        .filter(|(op, _)| op == "draft_answer_taken")
        .map(|(_, a)| a.clone())
        .collect();
    assert_eq!(taken.len(), 1);
    assert_eq!(
        (taken[0]["id"].as_str(), taken[0]["token"].as_str()),
        (Some("d1"), Some("lease-d"))
    );
}

fn pr_opened(risk: Option<&str>) -> Value {
    json!({"status": "pr_opened", "changeTitle": "Spec: Charts", "summary": "Charts", "risk": risk})
}

#[tokio::test]
async fn a_plan_someone_asked_for_merges_on_its_own() {
    let draft = json!({ "id": "d1", "request": "Add charts", "requestedBy": "ada" });
    let (mut rig, host, asked) = rig_with(draft, vec![pr_opened(None)]).await;
    let sent = reports(&mut rig, &asked, 3).await;
    assert_eq!(sent[1]["detail"], "awaiting_approval");
    assert_eq!(sent[2]["status"], "merged");
    assert_eq!(*host.merges.lock().unwrap(), 1);
}

#[tokio::test]
async fn a_risky_or_unrequested_plan_waits_for_a_person() {
    for (draft, risk) in [
        (
            json!({ "id": "d1", "requestedBy": "ada" }),
            Some("It changes how people sign in."),
        ),
        (json!({ "id": "d1" }), None),
        (
            json!({ "id": "d1", "requestedBy": "ada", "intake": { "trial": true } }),
            None,
        ),
    ] {
        let (mut rig, host, asked) = rig_with(draft, vec![pr_opened(risk)]).await;
        reports(&mut rig, &asked, 2).await;
        for _ in 0..20 {
            rig.machine.pass().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(*host.merges.lock().unwrap(), 0);
        assert!(!ops(&asked).contains(&"draft_take".to_string()));
    }
}

#[tokio::test]
async fn a_failed_plan_tries_again_once_and_then_waits_for_a_person() {
    let failed = json!({"status": "failed", "summary": "No luck"});
    let (mut rig, _, asked) = rig_with(claimed(), vec![failed.clone()]).await;
    let sent = reports(&mut rig, &asked, 2).await;
    assert_eq!(sent[1]["status"], "queued");
    assert_eq!(sent[1]["retried"], true);
    let again = json!({ "id": "d1", "request": "Add charts", "retried": true });
    let (mut rig, _, asked) = rig_with(again, vec![failed]).await;
    let sent = reports(&mut rig, &asked, 2).await;
    assert_eq!(sent[1]["status"], "needs_person");
    assert_eq!(sent[1]["detail"], "failed");
}

fn posts(asked: &Asked) -> Vec<Value> {
    (asked.lock().unwrap().iter())
        .filter(|(op, a)| op == "ask_report" && a["from"] == "retaskable")
        .map(|(_, a)| a.clone())
        .collect()
}

fn written() -> Value {
    json!({"status": "pr_opened", "changeTitle": "Spec: Charts", "summary": "Charts",
        "pieces": [{"title": "Charts", "kind": "feature"}]})
}

#[tokio::test]
async fn a_written_draft_asks_in_the_chat_that_wanted_it_once() {
    let draft = json!({ "id": "d1", "request": "Add charts", "askId": "a1" });
    let (mut rig, _host, asked) = rig_with(draft, vec![written()]).await;
    reports(&mut rig, &asked, 2).await;
    for _ in 0..20 {
        rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let sent = posts(&asked);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["draftId"], "d1");
    assert_eq!(sent[0]["replyTo"], "a1");
    assert!(
        sent[0]["text"]
            .as_str()
            .unwrap()
            .ends_with("Do you want to approve it?")
    );
}

#[tokio::test]
async fn an_add_spec_draft_asks_in_a_new_conversation() {
    let (mut rig, _host, asked) = rig_with(claimed(), vec![written()]).await;
    reports(&mut rig, &asked, 2).await;
    for _ in 0..20 {
        rig.machine.pass().await;
        if !posts(&asked).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let sent = posts(&asked);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].get("replyTo").is_none());
}

#[tokio::test]
async fn a_draft_with_questions_says_so_in_the_chat() {
    let draft = json!({ "id": "d1", "request": "Add charts", "askId": "a1" });
    let answer = json!({"status": "needs_clarification", "summary": "Too vague",
        "questions": [{"header": "Who", "question": "Who sees the charts?", "options": []}]});
    let (mut rig, _host, asked) = rig_with(draft, vec![answer]).await;
    reports(&mut rig, &asked, 2).await;
    for _ in 0..20 {
        rig.machine.pass().await;
        if !posts(&asked).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let sent = posts(&asked);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["replyTo"], "a1");
    assert_eq!(sent[0]["text"], crate::ask::follow_up::HAS_QUESTIONS);
}

fn out_for(secs: u64) -> std::time::Instant {
    std::time::Instant::now() + Duration::from_secs(secs)
}

/// The engines chosen as `choice`; with `claude_out` Claude is signed out, with `cursor_out`
/// Cursor is at its limit.
fn start_rules(rig: &mut Rig, choice: Value, claude_out: bool, cursor_out: bool) {
    rig.machine.links.engine_choice = serde_json::from_value(choice).unwrap();
    rig.machine.claude_signed_out_until = claude_out.then(|| out_for(600));
    if cursor_out {
        rig.machine
            .engine_limits
            .hit(crate::engines::EngineId::Cursor, out_for(600));
    }
}

fn charts() -> Value {
    json!({"status": "pr_opened", "changeTitle": "Spec: Charts", "changeBody": "Adds charts.",
        "itemIds": ["charts-1"], "summary": "Charts", "pieces": [{"title": "Charts", "kind": "feature"}],
        "breakdown": {"build": [{"id": "charts-1", "line": "Draw them", "size": "M"}], "people": []}})
}

#[tokio::test]
async fn with_claude_signed_out_a_draft_on_cursor_starts() {
    let (mut rig, host, asked) = rig_with(claimed(), vec![charts()]).await;
    start_rules(&mut rig, json!({"roles": {"drafter": "cursor"}}), true, false);
    reports(&mut rig, &asked, 2).await;
    assert_eq!(*host.engines.lock().unwrap(), vec![None]);
}

#[tokio::test]
async fn a_draft_on_claude_with_claude_out_starts_on_cursor_used_here() {
    let (mut rig, host, asked) = rig_with(claimed(), vec![charts()]).await;
    start_rules(&mut rig, json!({"roles": {"planner": "cursor"}}), true, false);
    reports(&mut rig, &asked, 2).await;
    assert_eq!(
        *host.engines.lock().unwrap(),
        vec![Some(crate::engines::EngineId::Cursor)]
    );
}

#[tokio::test]
async fn a_draft_waits_when_no_engine_may_start() {
    let (mut rig, host, asked) = rig_with(claimed(), vec![charts()]).await;
    start_rules(&mut rig, json!({"roles": {"planner": "cursor"}}), true, true);
    for _ in 0..3 {
        rig.machine.pass().await;
    }
    assert_eq!(claims(&asked), 0);
    assert!(host.jobs.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_draft_with_a_saved_session_waits_for_its_own_engine_instead_of_moving_to_cursor() {
    let draft = json!({"id": "d1", "request": "Add charts", "titles": [], "sessionId": "sess-9", "branch": "retaskable/spec-d1-x"});
    let (mut rig, host, asked) = rig_with(draft, vec![charts()]).await;
    start_rules(&mut rig, json!({"roles": {"planner": "cursor"}}), true, false);
    for _ in 0..3 {
        rig.machine.pass().await;
    }
    assert!(host.jobs.lock().unwrap().is_empty());
    assert!(ops(&asked).contains(&"draft_release".to_string()), "{:?}", ops(&asked));
}

#[tokio::test]
async fn a_draft_report_the_store_refuses_is_logged_as_a_store_error_for_the_draft() {
    let (mut rig, _host, _asked) = rig_failing(
        claimed(),
        vec![json!({"status": "failed", "summary": "x"})],
        "draft_report",
    )
    .await;
    for _ in 0..40 {
        rig.machine.pass().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let logged: Vec<Value> = (rig.lp(0).events().recent().iter())
        .map(|e| e.to_value())
        .filter(|v| v["kind"] == "store_error" && v["op"] == "draft_report")
        .collect();
    assert!(!logged.is_empty(), "{:?}", rig.lp(0).events().recent());
    assert_eq!(logged[0]["item"], "d1");
    assert_eq!(logged[0]["code"], "boom");
}

/// Claude at its usage limit holds a draft only without the model or sign-in.
#[tokio::test]
async fn at_the_limit_a_draft_starts_with_the_fallback_and_waits_without_it() {
    use crate::local_model::LocalModel;
    for (model, signed_out, starts) in [
        (LocalModel::up(), false, true),
        (LocalModel::off(), false, false),
        (LocalModel::up(), true, false),
    ] {
        let (mut rig, host, _asked) = rig_with(claimed(), vec![charts()]).await;
        start_rules(&mut rig, json!({}), signed_out, false);
        rig.machine.local = model;
        rig.machine
            .engine_limits
            .hit(crate::engines::EngineId::Claude, out_for(600));
        for _ in 0..4 {
            rig.machine.pass().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let case = signed_out;
        assert_eq!(host.engines.lock().unwrap().len(), usize::from(starts), "{case:?}");
    }
}

fn idea_plan() -> Value {
    json!({ "id": "d1", "request": "Add charts", "titles": ["Charts"], "intake": { "kinds": ["feature"] } })
}

#[tokio::test]
async fn a_bug_typed_as_an_idea_is_marked_and_planned_as_a_fix_on_its_first_claim() {
    let (mut rig, host, asked) = rig_with(idea_plan(), vec![json!({"status": "failed", "summary": "x"})]).await;
    host.bug.store(true, Ordering::SeqCst);
    reports(&mut rig, &asked, 2).await;
    assert!(ops(&asked).contains(&"idea_mark_bug".to_string()), "{:?}", ops(&asked));
    let requests = host.requests.lock().unwrap();
    assert!(
        requests[0].as_deref().unwrap().contains("Plan it as a fix"),
        "{requests:?}"
    );
}

#[tokio::test]
async fn an_idea_the_classifier_calls_an_idea_is_left_as_it_is() {
    let (mut rig, host, asked) = rig_with(idea_plan(), vec![json!({"status": "failed", "summary": "x"})]).await;
    reports(&mut rig, &asked, 2).await;
    assert!(!ops(&asked).contains(&"idea_mark_bug".to_string()));
    assert_eq!(host.requests.lock().unwrap()[0].as_deref(), Some("Add charts"));
}
