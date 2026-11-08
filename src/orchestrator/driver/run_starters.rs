use std::sync::Arc;

use super::{RunRequest, RunStarter};
use crate::backlog::runs::RunKind;

/// The starters `runs` mode has, as one: asks for the kinds any of them can run and hands a run to the one that can.
#[derive(Default)]
pub(super) struct RunStarters(pub(super) Vec<Arc<dyn RunStarter>>);

impl RunStarter for RunStarters {
    fn kinds(&self) -> Vec<RunKind> {
        let mut kinds: Vec<RunKind> = Vec::new();
        for kind in self.0.iter().flat_map(|s| s.kinds()) {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
        kinds
    }

    fn start(&self, request: RunRequest) -> Result<(), String> {
        match self.0.iter().find(|s| s.kinds().contains(&request.kind)) {
            Some(starter) => starter.start(request),
            None => Err(format!("no runner for {:?} runs", request.kind)),
        }
    }
}
