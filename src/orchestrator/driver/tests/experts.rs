//! The expert sessions (`driver/experts.rs`): a stateful fake store (the add and queue ops of a real one) and a fake
//! host whose turns are scripted.
use super::*;
use crate::config::ProjectConfig;
use crate::engines::engine::{EventKind, SessionEvent};
use crate::expert::claude_sessions::Continued;
use crate::expert::history::Turn;
use crate::expert::{Question, Reply};
use crate::implementer::publish::Published;
use crate::orchestrator::driver::{ExpertHost, ExpertJob, HelperJob, Opened, ShipJob, TurnEnd, WorkRead};
use crate::view::{ExpertAction, ExpertCommand};
use tokio::sync::Notify;
use tokio::sync::mpsc::UnboundedReceiver;

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
    /// Each helper started: (number, lane, own, from); whether they wait for `helper_go` (or their stop), and why they
    /// fail while that is set.
    helper_runs: Mutex<Vec<(u32, u32, bool, String)>>,
    helper_hold: AtomicBool,
    helper_go: Arc<Notify>,
    helper_fails: Mutex<Option<String>>,
    /// The turn takes messages as it works while set (a stand-in for a Claude turn with an open input): each message
    /// it is sent, whether it says it took them, and whether its input has closed already.
    steering: AtomicBool,
    steered: Arc<Mutex<Vec<String>>>,
    steer_takes: AtomicBool,
    steer_closed: AtomicBool,
    /// With a summary, the first message it takes ends a turn with that answer before the last.
    steer_turn: Mutex<Option<String>>,
    /// Turns end at the usage limit while set.
    limited: AtomicBool,
    /// The helpers the next turn asks for.
    asks: Mutex<Vec<crate::expert::helpers::Asked>>,
    /// What the work read answers, and each read asked: (lane, helpers).
    read: Mutex<WorkRead>,
    reads: Mutex<Vec<(Option<u32>, u32)>>,
}

impl ExpertHost for Host {
    fn turn(&self, mut job: ExpertJob) -> BoxFuture<'static, TurnEnd> {
        self.jobs.lock().unwrap().push(Ran {
            message: job.message.clone(),
            session_id: job.session_id.clone(),
            prepare: job.prepare,
            from_branch: job.from_branch,
            pictures_there: job.pictures.paths().iter().all(|p| p.is_file()),
        });
        let (hold, go) = (self.hold.load(Ordering::SeqCst), self.go.clone());
        let ask = self.question.lock().unwrap().take();
        let helpers = std::mem::take(&mut *self.asks.lock().unwrap());
        let limited = self.limited.load(Ordering::SeqCst);
        let steering = self.steering.load(Ordering::SeqCst);
        let (takes, closed) = (
            self.steer_takes.load(Ordering::SeqCst),
            self.steer_closed.load(Ordering::SeqCst),
        );
        let steered = self.steered.clone();
        let steer_turn = self.steer_turn.lock().unwrap().clone();
        async move {
            if steering && let Some(mut input) = job.steer.take() {
                if closed {
                    input.close();
                }
                loop {
                    tokio::select! {
                        () = go.notified() => break,
                        () = job.stop.notified() => return TurnEnd::Stopped,
                        Some(text) = input.next_message() => {
                            steered.lock().unwrap().push(text);
                            if let Some(said) = &steer_turn {
                                input.turn_ended(crate::engines::engine::SessionResult {
                                    ok: true,
                                    structured: Some(json!({ "summary": said })),
                                    ..Default::default()
                                });
                            }
                            if takes {
                                input.took();
                            }
                        }
                    }
                }
            } else if hold {
                tokio::select! {
                    () = go.notified() => {}
                    () = job.stop.notified() => return TurnEnd::Stopped,
                }
            }
            if limited {
                return TurnEnd::Limited("You've hit your limit".into());
            }
            TurnEnd::Done {
                session_id: job.session_id.unwrap_or_else(|| "sess-1".into()),
                reply: Reply {
                    summary: format!("Did: {}", job.message),
                    ask,
                    helpers,
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

    fn ship(&self, job: ShipJob) -> BoxFuture<'static, Result<Opened, String>> {
        self.shipped.lock().unwrap().push(job);
        let refused = self.ship_refused.lock().unwrap().clone();
        async move {
            match refused {
                Some(why) => Err(why),
                None => Ok(Opened {
                    change: Published {
                        url: "https://github.com/o/r/pull/9".into(),
                        number: 9,
                        created: true,
                    },
                    head: Some("a".repeat(40)),
                }),
            }
        }
        .boxed()
    }

    fn history(
        &self,
        _: &ProjectConfig,
        _: &str,
        session_id: Option<&str>,
        from: Option<&str>,
    ) -> BoxFuture<'static, (Vec<Turn>, Vec<Turn>)> {
        let said = |at: i64, text: &str, saying: Option<&str>| Turn {
            at,
            events: vec![SessionEvent {
                at,
                kind: EventKind::Text,
                text: text.into(),
            }],
            said: saying.map(str::to_string),
        };
        let earlier = from.map(|f| vec![said(10, &format!("earlier {f}"), Some("Fix it"))]);
        let own = session_id.map(|id| vec![said(100, &format!("first {id}"), None), said(200, "second", None)]);
        async move { (earlier.unwrap_or_default(), own.unwrap_or_default()) }.boxed()
    }

    fn helper(&self, job: HelperJob) -> BoxFuture<'static, crate::expert::helpers::Ended> {
        use crate::expert::helpers::Ended;
        self.helper_runs
            .lock()
            .unwrap()
            .push((job.n, job.lane, job.own, job.from.clone()));
        let (hold, go) = (self.helper_hold.load(Ordering::SeqCst), self.helper_go.clone());
        let fails = self.helper_fails.lock().unwrap().clone();
        async move {
            if hold {
                tokio::select! {
                    () = go.notified() => {}
                    () = job.stop.notified() => return Ended::Stopped,
                }
            }
            match fails {
                Some(why) => Ended::Failed(why),
                None => Ended::Finished(format!("found {}", job.task)),
            }
        }
        .boxed()
    }

    fn work(&self, _: &ProjectConfig, lane: Option<u32>, _: &str, helpers: u32) -> BoxFuture<'static, WorkRead> {
        self.reads.lock().unwrap().push((lane, helpers));
        let mut read = self.read.lock().unwrap().clone();
        // Without a lane only the pull requests are read.
        if lane.is_none() {
            read.work = None;
        }
        async move { read }.boxed()
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
mod fake_store;
use fake_store::{St, store};
mod carry;
mod helpers;
mod history;
mod limit;
mod pictures;
mod ship;
mod ship_runs;
mod steer;
mod wake;
mod work;

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

/// Another project's session holds this computer's added lane.
fn hold_added(x: &mut Ex) -> crate::lane_pool::Holder {
    let other = crate::lane_pool::Holder::Item {
        project: "p1".into(),
        item: "expert-other".into(),
    };
    assert!(x.rig.machine.pool.claim_added(other.clone()).is_some());
    other
}

#[tokio::test]
async fn with_the_added_lane_held_a_start_adds_a_parked_session_and_says_why() {
    let mut x = ex_lanes(1).await;
    let other = hold_added(&mut x);
    x.start("s1", "Fix it");
    let a = x.answer("s1").await;
    assert_eq!(
        (a["type"].as_str(), a["answer"]["itemId"].as_str()),
        (Some("ack"), Some(ID)),
        "{a}"
    );
    assert_eq!(x.st.count("add"), 1);
    x.quiet().await;
    assert_eq!((x.detail(ID).as_deref(), x.lane_of(ID)), (Some("expert_parked"), None));
    assert!(
        x.jobs().is_empty() && x.st.count("expert_queue_add") == 0,
        "the request isn't sent"
    );
    let said: Vec<String> = (x.st.args("report").iter())
        .filter_map(|n| n["note"].as_str().map(String::from))
        .collect();
    assert_eq!(
        said,
        [
            "Another expert session has this computer's added lane: park or finish it first. The session is parked and your request wasn't sent: send it again to try."
        ]
    );
    // A message is refused the same way until the lane is free, then takes it, on a fresh branch from the base.
    x.say("m1", ID, "again", false);
    let a = x.answer("m1").await;
    assert_eq!(
        a["reason"],
        "Another expert session has this computer's added lane: park or finish it first."
    );
    assert!(x.rig.machine.pool.release(x.rig.machine.pool.added_lane(), &other));
    x.say("m2", ID, "again", false);
    x.answer("m2").await;
    x.until("its turn", |x| x.jobs().len() == 1).await;
    assert!(x.jobs()[0].prepare && !x.jobs()[0].from_branch);
    assert_eq!(x.lane_of(ID), Some(2));
}

#[tokio::test]
async fn a_session_starts_in_the_added_lane_beside_the_lanes_and_gives_it_back_on_a_park() {
    let mut x = ex_lanes(1).await;
    x.quiet().await;
    assert_eq!(
        x.rig.live.snapshot().added_lane,
        Some(2),
        "the sheet is told it is free"
    );
    x.start("s1", "Fix it");
    x.answer("s1").await;
    x.until("idle", |x| x.detail(ID).as_deref() == Some("expert_idle"))
        .await;
    assert_eq!(x.lane_of(ID), Some(2), "one more than the computer's lanes");
    assert_eq!(x.rig.live.snapshot().added_lane, None, "and it is held now");
    let work = crate::lane_pool::Want {
        project: "p0".into(),
        item: "other".into(),
        chore: false,
        last_lane: None,
        fix_queued: false,
    };
    assert_eq!(x.rig.machine.pool.claim(&work), Some(1), "no lane sits out for it");
    x.end("e1", ID, "park");
    x.answer("e1").await;
    assert!(x.rig.machine.pool.added_free());
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
    assert_eq!(ex.lane_of(ID), Some(3), "the added lane, beside the two");
    assert_eq!(ex.rig.machine.pool.holder(1), Some(&run));
}

#[tokio::test]
async fn in_runs_mode_an_expert_start_while_the_added_lane_is_held_starts_parked() {
    let mut ex = ex_lanes(1).await;
    ex.rig.go_runs();
    let run = Holder::Run {
        project: "p0".into(),
        piece: "piece-1".into(),
    };
    assert!(ex.rig.machine.pool.take(1, run.clone()), "every lane held by runs");
    assert!(ex.rig.machine.pool.claim_added(run).is_some(), "and the added one");
    ex.start("c1", "Fix it");
    for _ in 0..5 {
        ex.rig.runs_pass().await;
    }
    ex.drain();
    let answer = ex.heard.iter().find(|a| a["id"] == "c1").cloned().unwrap();
    assert_eq!(answer["type"], "ack", "{answer}");
    assert!(ex.jobs().is_empty());
    assert_eq!(ex.detail(ID).as_deref(), Some("expert_parked"));
}
