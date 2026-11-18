//! The fake [`SessionStarter`] the driver tests run against.
use super::*;

impl SessionStarter for Fake {
    fn start(&self, request: StartRequest) -> Result<(), String> {
        self.started.lock().unwrap().push(request.clone());
        if self.panic_once.swap(false, Ordering::SeqCst) {
            panic!("a starter that goes wrong");
        }
        if let Some(why) = self.refuse.lock().unwrap().clone() {
            return Err(why);
        }
        let mut script = self.script.lock().unwrap();
        if !script.is_empty() {
            request.outcomes.send(SessionOutcome {
                project: request.project,
                item_id: request.item_id,
                lane: request.lane,
                result: script.remove(0),
                context_tokens: *self.context_tokens.lock().unwrap(),
                usage: None,
                notes: vec![],
                session_id: None,
            });
        }
        Ok(())
    }

    fn implementer_may_start(&self, pending: u32) -> bool {
        crate::orchestrator::session::model_has_room(self.slots.lock().unwrap().as_ref(), pending)
    }

    fn active(&self) -> u32 {
        self.active.load(Ordering::SeqCst)
    }

    fn play_lane(&self, dir: &std::path::Path, command: &str, lane: u32) -> Result<Box<dyn LanePlay>, String> {
        let closed = Arc::new(AtomicBool::new(false));
        self.games
            .lock()
            .unwrap()
            .push((dir.to_path_buf(), command.into(), lane, closed.clone()));
        Ok(Box::new(FakeGame(closed, self.stubborn_games.load(Ordering::SeqCst))))
    }

    fn abandon(&self, old: Abandon) -> BoxFuture<'static, Result<(), String>> {
        self.abandoned.lock().unwrap().push(old);
        let fails = self.abandon_fails.lock().unwrap().clone();
        async move { fails.map_or(Ok(()), Err) }.boxed()
    }

    fn landed(&self, _: &std::path::Path, _: u64, _: &str) -> BoxFuture<'static, Option<Option<String>>> {
        let landed = self.landed.lock().unwrap().clone();
        async move { landed }.boxed()
    }

    fn advance(&self, request: StepRequest) -> Option<BoxFuture<'static, crate::orchestrator::steps::StepDone>> {
        let run = request.run;
        self.stepped.lock().unwrap().push(request);
        if let Some(done) = self.done.lock().unwrap().take() {
            return Some(Box::pin(async move { done }));
        }
        Some(self.step(run))
    }
}

/// The task `health` sends for the item a claim hands out: its subject is the item's id (`TaskInput`).
pub(super) fn tasks_of(claim: &Value) -> Value {
    let id = claim["item"]["id"].as_str().unwrap_or("").to_string();
    json!([{ "id": format!("task-{id}"), "subject": id, "tier": "smart", "prompt": "" }])
}

/// The bodies of the `health` reports a store was sent.
pub(super) fn health_bodies(asked: &Asked) -> Vec<Value> {
    let all = asked.lock().unwrap();
    all.iter()
        .filter(|(op, _)| op == "health")
        .map(|(_, args)| args.clone())
        .collect()
}
/// A session starter that records what it was asked to start and step, and can panic once, refuse, or end each
/// session at once with the next scripted result.
#[derive(Default)]
pub(super) struct Fake {
    pub(super) started: Mutex<Vec<StartRequest>>,
    /// What Start over let go of, and why it can't, if set.
    pub(super) abandoned: Mutex<Vec<Abandon>>,
    pub(super) abandon_fails: Mutex<Option<String>>,
    pub(super) stepped: Mutex<Vec<StepRequest>>,
    pub(super) panic_once: AtomicBool,
    pub(super) refuse: Mutex<Option<String>>,
    pub(super) script: Mutex<Vec<SessionResult>>,
    /// The next scripted outcome's `context_tokens`.
    pub(super) context_tokens: Mutex<Option<u64>>,
    /// Sessions it says are running.
    pub(super) active: AtomicU32,
    /// Where a step leaves its item; `None` keeps its state.
    pub(super) to: Mutex<Option<ItemState>>,
    /// What the next steps hand back besides the run.
    pub(super) detail: Mutex<StepDetail>,
    /// The next step hands back this instead of its item's run.
    pub(super) done: Mutex<Option<crate::orchestrator::steps::StepDone>>,
    /// While set, a step never ends.
    pub(super) hold: AtomicBool,
    /// The next step panics.
    pub(super) step_panics: AtomicBool,
    /// Games started in a lane: the directory, the command, the lane, and whether it has closed.
    pub(super) games: Mutex<Vec<Game>>,
    /// Games that ignore `stop` (they close only when their flag is set).
    pub(super) stubborn_games: AtomicBool,
    /// The model slots `implementer_may_start` is asked against; none is no cap.
    pub(super) slots: Mutex<Option<crate::engines::model_slots::ModelSlots>>,
    pub(super) landed: Mutex<Option<Option<String>>>,
}

/// The directory, the command, the lane, and whether it has closed.
pub(super) type Game = (std::path::PathBuf, String, u32, Arc<AtomicBool>);

pub(super) struct FakeGame(Arc<AtomicBool>, bool);

impl LanePlay for FakeGame {
    fn finished(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    fn stop(&self) {
        if !self.1 {
            self.0.store(true, Ordering::SeqCst);
        }
    }
}

impl Fake {
    pub(super) fn step(&self, mut run: ItemRun) -> BoxFuture<'static, StepDone> {
        let detail = self.detail.lock().unwrap().clone();
        if let Some(to) = *self.to.lock().unwrap() {
            run.state = to;
        }
        let (hold, panics) = (
            self.hold.load(Ordering::SeqCst),
            self.step_panics.swap(false, Ordering::SeqCst),
        );
        async move {
            if hold {
                std::future::pending::<()>().await;
            }
            if panics {
                panic!("a step that goes wrong");
            }
            StepDone::from(run).with_detail(detail)
        }
        .boxed()
    }
}
