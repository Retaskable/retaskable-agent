//! The draft step (`driver/drafts.rs`): a fake store and a fake host whose drafter answers are scripted.
use super::*;
use crate::drafts::DrafterResult;
use crate::implementer::publish::Published;
use std::path::{Path, PathBuf};

use crate::config::ProjectConfig;
use crate::orchestrator::driver::{DraftHost, DraftJob};

/// What the fake host was asked, and what it answers.
#[derive(Default)]
struct Host {
    answers: Mutex<Vec<Value>>,
    jobs: Mutex<Vec<(String, bool, String)>>,
    published: Mutex<Vec<(String, String)>>,
    hold: AtomicBool,
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
        Ok(())
    }
    fn rebase(&self, _: &ProjectConfig, _: &str, _: &str) -> Result<bool> {
        Ok(true)
    }
    fn fetch(&self, _: &ProjectConfig) -> Result<()> {
        Ok(())
    }
}

/// A store whose `draft_claim_next` hands out `draft` once; every op is recorded.
async fn store(draft: Value) -> (String, Asked) {
    let asked: Asked = Default::default();
    let (seen, handed) = (asked.clone(), Arc::new(AtomicBool::new(false)));
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, handed, draft) = (seen.clone(), handed.clone(), draft.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let value = match op.as_str() {
                    "list" => json!([]),
                    "draft_claim_next" if !handed.swap(true, Ordering::SeqCst) => {
                        json!({ "draft": draft, "token": "lease-d" })
                    }
                    _ => Value::Null,
                };
                Json(json!({ "value": value }))
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
    let (base, asked) = store(draft).await;
    let mut rig = rig(2, &[&base]);
    rig.go_live();
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
    let (mut rig, host, asked) = rig_with(claimed(), vec![json!({"status": "failed", "summary": "No luck"})]).await;
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
