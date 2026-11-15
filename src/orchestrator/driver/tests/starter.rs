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

    fn triage(&self, request: TriageRequest) -> Result<(), String> {
        self.triage.start(request)
    }

    fn second_look(&self, request: SecondLookRequest) -> Result<(), String> {
        self.triage.second_look(request)
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

    fn advance(&self, request: StepRequest) -> Option<BoxFuture<'static, crate::orchestrator::steps::StepDone>> {
        let run = request.run;
        self.stepped.lock().unwrap().push(request);
        Some(self.step(run))
    }
}
