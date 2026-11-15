//! The rig in `runs` mode, for the tests of the passes that run there.
use super::*;

impl Rig {
    /// Puts every project in `runs` mode, with a runner for watch runs (which hold no lane and no session), and takes
    /// the claim starter away.
    pub(super) fn go_runs(&mut self) {
        struct Watch;
        impl RunStarter for Watch {
            fn kinds(&self) -> Vec<crate::backlog::runs::RunKind> {
                vec![crate::backlog::runs::RunKind::Watch]
            }
            fn start(&self, _: RunRequest) -> Result<(), String> {
                Ok(())
            }
        }
        self.machine.starter = None;
        self.machine.run_starters = run_starters::RunStarters(vec![Arc::new(Watch)]);
        for lp in &mut self.machine.projects {
            lp.mode = OrchestratorMode::Runs;
        }
    }

    /// A pass that reads the backlogs, then the run loop's passes.
    pub(super) async fn runs_pass(&mut self) {
        self.machine.pass().await;
        self.machine.run_passes().await;
    }
}
