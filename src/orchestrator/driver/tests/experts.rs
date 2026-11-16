//! The expert sessions (`driver/experts.rs`): a stateful fake store (the add and queue ops of a real one) and a fake
//! host whose turns are scripted.
use super::*;
use crate::config::ProjectConfig;
use crate::expert::claude_sessions::Continued;
use crate::expert::{Question, Reply};
use crate::implementer::publish::Published;
use crate::orchestrator::driver::{ExpertHost, ExpertJob, ShipJob, TurnEnd};
use crate::view::{ExpertAction, ExpertCommand};
use axum::http::StatusCode;
use std::collections::BTreeSet;
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedReceiver;

#[derive(Default)]
struct St {
    ops: Asked,
    rows: Mutex<Vec<Value>>,
    /// The continuation notes `expert_continues_set` wrote, by item.
    notes: Mutex<serde_json::Map<String, Value>>,
    /// Ids `add` refuses as taken.
    taken_ids: Mutex<BTreeSet<String>>,
    seq: AtomicU32,
    /// What `draft_queue` and `release` are refused with (a 4xx code), while set.
    queue_refused: AtomicBool,
    release_refused: Mutex<Option<&'static str>>,
}

impl St {
    fn handle(&self, body: Value) -> (StatusCode, Json<Value>) {
        let op = body["op"].as_str().unwrap_or("").to_string();
        let args = body["args"].clone();
        self.ops.lock().unwrap().push((op.clone(), args.clone()));
        let ok = |v: Value| (StatusCode::OK, Json(json!({ "value": v })));
        let conflict = || {
            (
                StatusCode::CONFLICT,
                Json(json!({ "error": "conflict", "message": "taken" })),
            )
        };
        let item = args["itemId"].as_str().unwrap_or("").to_string();
        let mut rows = self.rows.lock().unwrap();
        match op.as_str() {
            "list" => ok(json!([])),
            "facts_get" => ok(json!({ "paused": false })),
            "add" => {
                let it = &args["items"][0];
                let id = it["id"].as_str().unwrap_or("").to_string();
                if !self.taken_ids.lock().unwrap().insert(id.clone()) {
                    return conflict();
                }
                let item = json!({ "id": id, "title": it["title"], "status": "claimed", "rank": 1 });
                ok(json!({ "item": item, "token": format!("lease-{id}") }))
            }
            "expert_queue_add" => {
                let n = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
                let at = crate::telemetry::now_ms() + i64::from(n);
                let id = format!("q-{n}");
                rows.push(json!({
                    "itemId": item, "id": id, "text": args["text"], "at": at, "by": args["by"], "state": "queued",
                    "pictureIds": args.get("pictureIds").cloned().unwrap_or(json!([]))
                }));
                ok(json!({ "id": id }))
            }
            "expert_queue_take" => {
                let ids: Vec<String> = serde_json::from_value(args["ids"].clone()).unwrap_or_default();
                let mut got = vec![];
                for id in ids {
                    match rows.iter_mut().find(|r| r["id"] == id && r["itemId"] == item) {
                        Some(r) => {
                            r["state"] = json!("taken");
                            got.push(r.clone());
                        }
                        None => return conflict(),
                    }
                }
                ok(json!({ "rows": got }))
            }
            "expert_queue_done" => {
                let ids: Vec<Value> = args["ids"].as_array().cloned().unwrap_or_default();
                rows.retain(|r| !(r["itemId"] == item && ids.contains(&r["id"])));
                ok(json!({ "ok": true }))
            }
            "expert_continues_set" => {
                self.notes.lock().unwrap().insert(item, args["from"].clone());
                ok(Value::Null)
            }
            "draft_queue" if self.queue_refused.load(Ordering::SeqCst) => conflict(),
            "draft_queue" => ok(json!({ "draft": { "id": "draft-1" } })),
            "release" if self.release_refused.lock().unwrap().is_some() => {
                let code = self.release_refused.lock().unwrap().unwrap();
                let status = if code == "stale_lease" {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                };
                (status, Json(json!({ "error": code, "message": code })))
            }
            "revise" => ok(json!({ "revised": "applied", "status": "todo" })),
            "expert_queue_clear" => {
                rows.retain(|r| r["itemId"] != item);
                ok(Value::Null)
            }
            "expert_queue_list" => {
                let mut sessions = serde_json::Map::new();
                for r in rows.iter() {
                    let k = r["itemId"].as_str().unwrap_or("").to_string();
                    sessions
                        .entry(k)
                        .or_insert_with(|| json!([]))
                        .as_array_mut()
                        .unwrap()
                        .push(r.clone());
                }
                ok(json!({ "sessions": sessions, "continues": *self.notes.lock().unwrap() }))
            }
            _ => ok(Value::Null),
        }
    }

    fn count(&self, op: &str) -> usize {
        self.ops.lock().unwrap().iter().filter(|(o, _)| o == op).count()
    }

    fn args(&self, op: &str) -> Vec<Value> {
        (self.ops.lock().unwrap().iter())
            .filter(|(o, _)| o == op)
            .map(|(_, a)| a.clone())
            .collect()
    }

    /// A message the site wrote, which this computer didn't.
    fn site_says(&self, item: &str, text: &str) {
        let n = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.rows.lock().unwrap().push(json!({
            "itemId": item, "id": format!("q-{n}"), "text": text,
            "at": crate::telemetry::now_ms() + i64::from(n), "by": "ada", "state": "queued"
        }));
    }

    fn cancel(&self, id: &str) {
        self.rows.lock().unwrap().retain(|r| r["id"] != id);
    }
}

async fn store() -> (String, Arc<St>) {
    let st = Arc::new(St::default());
    let s = st.clone();
    let app = Router::new()
        .route(
            "/v1/repos/{id}/ops",
            post(move |Json(body): Json<Value>| {
                let s = s.clone();
                async move { s.handle(body) }
            }),
        )
        // Every picture the store is asked for is its own name, but `0…ff`, which it doesn't have.
        .route(
            "/v1/repos/{id}/pictures/{picture}",
            axum::routing::get(
                |axum::extract::Path((_, picture)): axum::extract::Path<(u64, String)>| async move {
                    if picture.contains("-0000-4000-8000-0000000000ff") {
                        (StatusCode::NOT_FOUND, b"{}".to_vec())
                    } else {
                        (StatusCode::OK, picture.into_bytes())
                    }
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), st)
}

#[derive(Debug, Clone)]
struct Ran {
    message: String,
    session_id: Option<String>,
    prepare: bool,
    from_branch: bool,
    /// Whether every picture the message lists was on disk while the turn ran.
    pictures_there: bool,
}

#[derive(Default)]
struct Host {
    jobs: Mutex<Vec<Ran>>,
    /// Turns that start now wait for `go`, or for their stop.
    hold: AtomicBool,
    go: Arc<Notify>,
    parks: Mutex<Vec<String>>,
    park_fails: AtomicBool,
    discards: Mutex<Vec<(Option<u32>, String)>>,
    /// Each `ship` asked, and why it is refused while set.
    shipped: Mutex<Vec<ShipJob>>,
    ship_refused: Mutex<Option<String>>,
    question: Mutex<Option<Question>>,
    /// The Claude Code sessions of this computer, by id.
    claude: Mutex<std::collections::BTreeMap<String, Continued>>,
    /// Whether the branch of a continued session is carried on, or why not, and each `carry_on` asked: (name, folder).
    carry: Mutex<Option<String>>,
    carried: Mutex<Vec<(String, String)>>,
    /// Whether a continued session's file is still readable for its handover.
    file_gone: AtomicBool,
}

impl ExpertHost for Host {
    fn turn(&self, job: ExpertJob) -> BoxFuture<'static, TurnEnd> {
        self.jobs.lock().unwrap().push(Ran {
            message: job.message.clone(),
            session_id: job.session_id.clone(),
            prepare: job.prepare,
            from_branch: job.from_branch,
            pictures_there: job.pictures.paths().iter().all(|p| p.is_file()),
        });
        let (hold, go) = (self.hold.load(Ordering::SeqCst), self.go.clone());
        let ask = self.question.lock().unwrap().take();
        async move {
            if hold {
                tokio::select! {
                    () = go.notified() => {}
                    () = job.stop.notified() => return TurnEnd::Stopped,
                }
            }
            TurnEnd::Done {
                session_id: job.session_id.unwrap_or_else(|| "sess-1".into()),
                reply: Reply {
                    summary: format!("Did: {}", job.message),
                    ask,
                },
            }
        }
        .boxed()
    }

    fn park(&self, _: &ProjectConfig, _: u32, branch: &str) -> BoxFuture<'static, Result<(), String>> {
        self.parks.lock().unwrap().push(branch.to_string());
        let fails = self.park_fails.load(Ordering::SeqCst);
        async move { if fails { Err("push refused".into()) } else { Ok(()) } }.boxed()
    }

    fn continuing(&self, id: &str) -> Result<Continued, String> {
        (self.claude.lock().unwrap().get(id).cloned())
            .ok_or_else(|| "That Claude Code session isn't on this computer any more.".to_string())
    }

    fn carry_on(&self, _: &ProjectConfig, from: &Continued, name: &str) -> BoxFuture<'static, Result<(), String>> {
        self.carried
            .lock()
            .unwrap()
            .push((name.to_string(), from.folder.clone()));
        let why = self.carry.lock().unwrap().clone();
        async move { why.map_or(Ok(()), Err) }.boxed()
    }

    fn handover(&self, id: &str, request: &str) -> BoxFuture<'static, Option<String>> {
        let made = (!self.file_gone.load(Ordering::SeqCst)).then(|| format!("HANDOVER of {id}\n{request}"));
        async move { made }.boxed()
    }

    fn ship(&self, job: ShipJob) -> BoxFuture<'static, Result<Published, String>> {
        self.shipped.lock().unwrap().push(job);
        let refused = self.ship_refused.lock().unwrap().clone();
        async move {
            match refused {
                Some(why) => Err(why),
                None => Ok(Published {
                    url: "https://github.com/o/r/pull/9".into(),
                    number: 9,
                    created: true,
                }),
            }
        }
        .boxed()
    }

    fn discard(&self, _: &ProjectConfig, lane: Option<u32>, branch: &str) -> BoxFuture<'static, ()> {
        self.discards.lock().unwrap().push((lane, branch.to_string()));
        async {}.boxed()
    }
}

struct Ex {
    rig: Rig,
    host: Arc<Host>,
    st: Arc<St>,
    answers: UnboundedReceiver<Value>,
    heard: Vec<Value>,
}

async fn ex() -> Ex {
    ex_lanes(2).await
}

async fn ex_lanes(lanes: u32) -> Ex {
    let (base, st) = store().await;
    let mut rig = rig(lanes, &[&base]);
    rig.go_live();
    let host = Arc::new(Host::default());
    rig.machine.experts = Some(host.clone());
    let (tx, answers) = tokio::sync::mpsc::unbounded_channel();
    rig.machine.links.item_commands.set_out("p0", Some(tx));
    Ex {
        rig,
        host,
        st,
        answers,
        heard: vec![],
    }
}

impl Ex {
    fn send(&self, id: &str, by: &str, action: ExpertAction) {
        self.rig.machine.links.item_commands.push_expert(ExpertCommand {
            project: "p0".into(),
            command_id: id.into(),
            by: by.into(),
            action,
        });
    }

    fn start(&self, id: &str, request: &str) {
        self.send(
            id,
            "ada",
            ExpertAction::Start {
                request: request.into(),
                continue_from: None,
            },
        );
    }

    fn say(&self, id: &str, item: &str, text: &str, now: bool) {
        self.send(
            id,
            "ada",
            ExpertAction::Say {
                item: item.into(),
                text: text.into(),
                pictures: vec![],
                now,
            },
        );
    }

    fn end(&self, id: &str, item: &str, outcome: &str) {
        self.send(
            id,
            "ada",
            ExpertAction::End {
                item: item.into(),
                outcome: outcome.into(),
            },
        );
    }

    fn drain(&mut self) {
        while let Ok(a) = self.answers.try_recv() {
            self.heard.push(a);
        }
    }

    /// The answer to command `id`, once it is there.
    async fn answer(&mut self, id: &str) -> Value {
        for _ in 0..300 {
            self.rig.machine.pass().await;
            self.drain();
            if let Some(a) = self.heard.iter().find(|a| a["id"] == id) {
                return a.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no answer to {id}: {:?}", self.heard);
    }

    async fn until(&mut self, what: &str, ok: impl Fn(&Ex) -> bool) {
        for _ in 0..300 {
            self.rig.machine.pass().await;
            self.drain();
            if ok(self) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("never: {what}");
    }

    async fn quiet(&mut self) {
        for _ in 0..8 {
            self.rig.machine.pass().await;
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        self.drain();
    }

    fn expert(&self, item: &str) -> Option<Value> {
        let live = self.rig.live.snapshot();
        let items = &live.projects.get("p0")?.items;
        items.iter().find(|i| i.id == item)?.expert.clone()
    }

    fn detail(&self, item: &str) -> Option<String> {
        self.expert(item)?["detail"].as_str().map(String::from)
    }

    fn jobs(&self) -> Vec<Ran> {
        self.host.jobs.lock().unwrap().clone()
    }

    fn lane_of(&self, item: &str) -> Option<u32> {
        self.rig.machine.pool.lane_of("p0", item)
    }
}

const ID: &str = "expert-fix-it";

mod backlog;
mod carry;
mod pictures;
mod ship;
mod wake;

/// Starts "Fix it" and waits for its first turn to be taken in.
async fn started(x: &mut Ex) {
    x.start("s1", "Fix it");
    let a = x.answer("s1").await;
    assert_eq!(a["type"], "ack", "{a}");
    x.until("the first turn", |x| x.detail(ID).as_deref() == Some("expert_idle"))
        .await;
}

#[tokio::test]
async fn a_start_adds_the_session_runs_its_first_turn_and_answers_with_the_id() {
    let mut x = ex().await;
    x.start("s1", "Fix it");
    let a = x.answer("s1").await;
    assert_eq!(
        (a["type"].as_str(), a["answer"]["itemId"].as_str()),
        (Some("ack"), Some(ID))
    );
    x.until("the first turn", |x| x.detail(ID).as_deref() == Some("expert_idle"))
        .await;
    let add = &x.st.args("add")[0];
    assert_eq!(add["via"], "expert");
    assert_eq!(add["startedBy"], "ada");
    assert_eq!(add["items"][0]["id"], ID);
    assert_eq!(add["items"][0]["remaining"], "Fix it");
    let jobs = x.jobs();
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        (jobs[0].message.as_str(), jobs[0].prepare, jobs[0].from_branch),
        ("Fix it", true, false)
    );
    assert_eq!(jobs[0].session_id, None);
    assert!(x.lane_of(ID).is_some(), "the session holds a lane");
    // The answer went to the item's history, and the row was read.
    let notes = x.st.args("report");
    assert!(
        notes
            .iter()
            .any(|n| n["note"] == "Did: Fix it" && n["token"] == format!("lease-{ID}")),
        "{notes:?}"
    );
    assert_eq!(x.st.count("expert_queue_done"), 1);
    assert_eq!(x.st.rows.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn a_taken_id_moves_the_session_to_the_next_one() {
    let mut x = ex().await;
    x.st.taken_ids.lock().unwrap().insert(ID.into());
    x.start("s1", "Fix it");
    let a = x.answer("s1").await;
    assert_eq!(a["answer"]["itemId"], "expert-fix-it-2");
}

#[tokio::test]
async fn a_later_message_resumes_the_session_with_just_its_words() {
    let mut x = ex().await;
    started(&mut x).await;
    x.say("m1", ID, "and the tests", false);
    assert_eq!(x.answer("m1").await["type"], "ack");
    x.until("the second turn", |x| {
        x.jobs().len() == 2 && x.detail(ID).as_deref() == Some("expert_idle")
    })
    .await;
    let j = &x.jobs()[1];
    assert_eq!(
        (j.message.as_str(), j.session_id.as_deref(), j.prepare),
        ("and the tests", Some("sess-1"), false)
    );
}

#[tokio::test]
async fn messages_sent_mid_turn_wait_in_order_and_run_one_turn_each() {
    let mut x = ex().await;
    x.host.hold.store(true, Ordering::SeqCst);
    x.start("s1", "Fix it");
    x.until("the turn running", |x| x.jobs().len() == 1).await;
    x.say("m1", ID, "first", false);
    x.say("m2", ID, "second", false);
    x.until("both queued", |x| {
        x.expert(ID)
            .is_some_and(|e| e["queue"].as_array().is_some_and(|q| q.len() == 2))
    })
    .await;
    let e = x.expert(ID).unwrap();
    assert_eq!(e["detail"], "expert_working");
    assert_eq!(e["queue"][0]["text"], "first");
    assert_eq!(e["queue"][1]["text"], "second");
    assert!(e["queue"][0]["at"].as_str().unwrap().contains('T'));
    x.host.hold.store(false, Ordering::SeqCst);
    x.host.go.notify_one();
    x.until("all three turns", |x| {
        x.jobs().len() == 3 && x.detail(ID).as_deref() == Some("expert_idle")
    })
    .await;
    let said: Vec<String> = x.jobs().iter().map(|j| j.message.clone()).collect();
    assert_eq!(said, ["Fix it", "first", "second"]);
}

#[tokio::test]
async fn stopping_cuts_the_turn_keeps_the_queue_and_the_next_message_says_so() {
    let mut x = ex().await;
    x.host.hold.store(true, Ordering::SeqCst);
    x.start("s1", "Fix it");
    x.until("the turn running", |x| x.jobs().len() == 1).await;
    x.say("m1", ID, "queued", false);
    x.send("st", "ada", ExpertAction::Stop { item: ID.into() });
    assert_eq!(x.answer("st").await["type"], "ack");
    x.until("idle", |x| x.detail(ID).as_deref() == Some("expert_idle"))
        .await;
    x.quiet().await;
    assert_eq!(x.jobs().len(), 1, "a stopped session runs nothing by itself");
    assert_eq!(x.expert(ID).unwrap()["queue"][0]["text"], "queued");
    x.host.hold.store(false, Ordering::SeqCst);
    x.say("m2", ID, "go on", false);
    x.until("both turns", |x| {
        x.jobs().len() == 3 && x.detail(ID).as_deref() == Some("expert_idle")
    })
    .await;
    let jobs = x.jobs();
    assert!(jobs[1].message.starts_with(crate::expert::MID_STEP) && jobs[1].message.ends_with("queued"));
    assert_eq!(jobs[2].message, "go on");
}

#[tokio::test]
async fn saying_now_cuts_the_turn_and_runs_the_message_next() {
    let mut x = ex().await;
    x.host.hold.store(true, Ordering::SeqCst);
    x.start("s1", "Fix it");
    x.until("the turn running", |x| x.jobs().len() == 1).await;
    x.host.hold.store(false, Ordering::SeqCst);
    x.say("m1", ID, "wait, no", true);
    x.until("the second turn", |x| {
        x.jobs().len() == 2 && x.detail(ID).as_deref() == Some("expert_idle")
    })
    .await;
    assert!(x.jobs()[1].message.ends_with("wait, no"));
}

#[tokio::test]
async fn only_the_person_who_started_a_session_drives_it() {
    let mut x = ex().await;
    started(&mut x).await;
    x.send(
        "m1",
        "bob",
        ExpertAction::Say {
            item: ID.into(),
            text: "hi".into(),
            pictures: vec![],
            now: false,
        },
    );
    let a = x.answer("m1").await;
    assert_eq!(a["type"], "nack");
    assert!(a["reason"].as_str().unwrap().contains("Only ada"), "{a}");
    x.say("m2", "expert-nothing", "hi", false);
    assert_eq!(x.answer("m2").await["type"], "nack");
    assert_eq!(x.jobs().len(), 1);
}

#[tokio::test]
async fn a_question_shows_as_asking_with_its_options() {
    let mut x = ex().await;
    *x.host.question.lock().unwrap() = Some(Question {
        question: "Which?".into(),
        options: vec![("A".into(), None), ("B".into(), Some("second".into()))],
    });
    x.start("s1", "Fix it");
    x.until("asking", |x| x.detail(ID).as_deref() == Some("expert_asking"))
        .await;
    let e = x.expert(ID).unwrap();
    assert_eq!(e["ask"]["question"], "Which?");
    assert_eq!(e["ask"]["options"][1], json!({ "label": "B", "detail": "second" }));
    assert!(
        x.st.args("report")
            .iter()
            .any(|n| n["note"] == "Did: Fix it\nAsks: Which?")
    );
}

#[tokio::test]
async fn ending_with_a_discard_clears_the_queue_lets_the_item_go_and_frees_the_lane() {
    let mut x = ex().await;
    started(&mut x).await;
    let lane = x.lane_of(ID).unwrap();
    x.end("e1", ID, "discard");
    assert_eq!(x.answer("e1").await["type"], "ack");
    assert_eq!(
        (
            x.st.count("expert_queue_clear"),
            x.st.count("release"),
            x.st.count("skip")
        ),
        (1, 1, 1)
    );
    assert_eq!(
        x.st.args("release")[0],
        json!({ "id": ID, "token": format!("lease-{ID}") })
    );
    assert_eq!(
        *x.host.discards.lock().unwrap(),
        vec![(Some(lane), "retaskable/expert-fix-it-fix-it".to_string())]
    );
    assert_eq!(x.lane_of(ID), None);
    assert!(x.expert(ID).is_none(), "gone from the snapshot");
}

#[tokio::test]
async fn parking_pushes_the_branch_frees_the_lane_and_the_next_message_comes_back_on_it() {
    let mut x = ex().await;
    started(&mut x).await;
    x.end("e1", ID, "park");
    assert_eq!(x.answer("e1").await["type"], "ack");
    assert_eq!(
        *x.host.parks.lock().unwrap(),
        vec!["retaskable/expert-fix-it-fix-it".to_string()]
    );
    assert_eq!((x.lane_of(ID), x.detail(ID).as_deref()), (None, Some("expert_parked")));
    assert_eq!(x.st.count("release"), 0, "a parked session keeps its lease");
    x.say("m1", ID, "back again", false);
    x.until("the turn after the park", |x| {
        x.jobs().len() == 2 && x.detail(ID).as_deref() == Some("expert_idle")
    })
    .await;
    let j = &x.jobs()[1];
    assert_eq!(
        (j.message.as_str(), j.prepare, j.from_branch),
        ("back again", true, true)
    );
    assert_eq!(j.session_id, None, "a new Claude session on the pushed branch");
    assert!(x.lane_of(ID).is_some());
}

#[tokio::test]
async fn a_push_that_fails_keeps_the_lane() {
    let mut x = ex().await;
    started(&mut x).await;
    x.host.park_fails.store(true, Ordering::SeqCst);
    x.end("e1", ID, "park");
    let a = x.answer("e1").await;
    assert_eq!(a["type"], "nack");
    assert!(a["reason"].as_str().unwrap().contains("push refused"));
    assert!(x.lane_of(ID).is_some());
    assert_eq!(x.detail(ID).as_deref(), Some("expert_idle"));
}

#[tokio::test]
async fn an_idle_session_parks_after_the_idle_time_and_a_busy_one_does_not() {
    let mut x = ex().await;
    x.rig.lp(0).experts.idle = Duration::from_millis(150);
    started(&mut x).await;
    x.until("parked", |x| x.detail(ID).as_deref() == Some("expert_parked"))
        .await;
    assert_eq!(x.lane_of(ID), None);
    assert_eq!(x.host.parks.lock().unwrap().len(), 1);
    // A turn running is not idle.
    x.host.hold.store(true, Ordering::SeqCst);
    x.say("m1", ID, "again", false);
    x.until("running", |x| x.jobs().len() == 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    x.quiet().await;
    assert_eq!(x.detail(ID).as_deref(), Some("expert_working"));
    assert_eq!(x.host.parks.lock().unwrap().len(), 1);
    x.host.go.notify_one();
}

#[tokio::test]
async fn the_beat_touches_the_lease_and_reads_the_store_queue_back() {
    let mut x = ex().await;
    x.rig.lp(0).experts.beat_every = Duration::ZERO;
    x.host.hold.store(true, Ordering::SeqCst);
    x.start("s1", "Fix it");
    x.until("the turn running", |x| x.jobs().len() == 1).await;
    x.say("m1", ID, "mine", false);
    x.until("queued", |x| x.expert(ID).is_some_and(|e| e["queue"].is_array()))
        .await;
    // The site cancels this computer's row and writes one of its own.
    let mine = x.st.rows.lock().unwrap().iter().find(|r| r["text"] == "mine").unwrap()["id"].clone();
    x.st.cancel(mine.as_str().unwrap());
    x.st.site_says(ID, "from the site");
    x.until("the site's row", |x| {
        x.expert(ID).is_some_and(|e| {
            e["queue"]
                .as_array()
                .is_some_and(|q| q.len() == 1 && q[0]["text"] == "from the site")
        })
    })
    .await;
    let beats: Vec<Value> =
        x.st.args("report")
            .into_iter()
            .filter(|a| a.get("note").is_none())
            .collect();
    assert!(
        beats
            .iter()
            .any(|b| b == &json!({ "id": ID, "token": format!("lease-{ID}") })),
        "{beats:?}"
    );
    x.host.hold.store(false, Ordering::SeqCst);
    x.host.go.notify_one();
    x.until("the site's message run", |x| x.jobs().len() == 2).await;
    assert_eq!(x.jobs()[1].message, "from the site");
}

#[tokio::test]
async fn with_no_lane_free_a_start_is_refused() {
    let mut x = ex_lanes(1).await;
    let other = crate::lane_pool::Want {
        project: "p0".into(),
        item: "other".into(),
        chore: false,
        last_lane: None,
        fix_queued: false,
    };
    assert!(x.rig.machine.pool.claim(&other).is_some());
    x.start("s1", "Fix it");
    let a = x.answer("s1").await;
    assert_eq!(a["type"], "nack");
    assert!(a["reason"].as_str().unwrap().contains("No lane"), "{a}");
    assert_eq!(x.st.count("add"), 0);
}

#[tokio::test]
async fn without_a_link_to_the_store_nothing_starts() {
    let mut x = ex().await;
    *x.rig.keys.token.lock().unwrap() = None;
    x.start("s1", "Fix it");
    x.quiet().await;
    assert_eq!((x.st.count("add"), x.jobs().len()), (0, 0));
    assert!(x.lane_of(ID).is_none());
}

#[tokio::test]
async fn an_expert_item_in_the_store_is_never_planned_by_the_loop() {
    assert!(crate::expert::is_expert("expert-x", None));
    let items: Vec<crate::backlog::Item> = serde_json::from_value(json!([
        { "id": "expert-x", "title": "t", "status": "claimed", "rank": 1 },
        { "id": "a", "title": "t", "status": "claimed", "rank": 2, "kind": "expert" },
        { "id": "b", "title": "t", "status": "claimed", "rank": 3 },
        { "id": "expert-y", "title": "t", "status": "in_review", "rank": 4, "detail": "expert_shipped" }
    ]))
    .unwrap();
    let kept: Vec<_> = items
        .iter()
        .filter(|i| !crate::expert::held_by_experts(&i.id, i.kind.as_deref(), i.detail.as_deref()))
        .map(|i| i.id.as_str())
        .collect();
    // A session whose change is open goes on as any item does.
    assert_eq!(kept, ["b", "expert-y"]);
}

#[tokio::test]
async fn in_runs_mode_an_expert_session_takes_a_lane_no_run_holds_and_its_turn_runs() {
    let mut ex = ex().await;
    ex.rig.go_runs();
    let run = Holder::Run {
        project: "p0".into(),
        piece: "piece-1".into(),
    };
    assert!(ex.rig.machine.pool.take(1, run.clone()));
    ex.start("c1", "Fix it");
    for _ in 0..300 {
        ex.rig.runs_pass().await;
        ex.drain();
        if ex.heard.iter().any(|a| a["id"] == "c1") && !ex.jobs().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let answer = ex.heard.iter().find(|a| a["id"] == "c1").cloned().unwrap();
    assert_eq!(answer["type"], "ack", "{answer}");
    assert_eq!(ex.jobs().len(), 1);
    assert_eq!(ex.lane_of(ID), Some(2));
    assert_eq!(ex.rig.machine.pool.holder(1), Some(&run));
}

#[tokio::test]
async fn in_runs_mode_an_expert_start_with_every_lane_held_by_runs_is_refused() {
    let mut ex = ex_lanes(1).await;
    ex.rig.go_runs();
    let run = Holder::Run {
        project: "p0".into(),
        piece: "piece-1".into(),
    };
    assert!(ex.rig.machine.pool.take(1, run));
    ex.start("c1", "Fix it");
    for _ in 0..5 {
        ex.rig.runs_pass().await;
    }
    ex.drain();
    let answer = ex.heard.iter().find(|a| a["id"] == "c1").cloned().unwrap();
    assert_eq!(answer["type"], "nack");
    assert!(ex.jobs().is_empty());
}
