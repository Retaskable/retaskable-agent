//! What a push wakes. Nothing is run from a push: it ends a loop's wait so the loop reads the store now instead of at
//! its next beat or poll (the store's reply, as ever, is what it acts on).
//!
//! `storeLink.ts`'s `pushed` does more (settings, pause, try-outs, ideas, drafts, facts, the intent puller); this agent
//! wakes only the loops that poll slower than a person waits:
//! - `work_available`: the run loop asks for work again.
//! - `facts_changed` naming `expert_queue` (an edit or a cancel of a queued message on the site, or another computer
//!   taking one), `deploy_requests` or `intents` (the site wrote a row), or naming nothing (any may have changed): the
//!   expert sessions read their queue back, the deploy loop lists its requests, the intent puller of Add spec, Build this
//!   and the reads lists its rows (a read row times out after 30 seconds unclaimed, so a minute's poll is too slow).
//! - `reconnected` (the socket was down, so a push may have been missed): all four.
//!
//! The rest are heard and dropped: the other intents are pulled every poll already, and the pause, the settings and the
//! facts are read every minute.
use super::Push;
use crate::work_signal::{Wake, WorkSignals};

/// Whether `kinds` of a `facts_changed` include `kind`: none named means any.
fn names(kinds: &[String], kind: &str) -> bool {
    kinds.is_empty() || kinds.iter().any(|k| k == kind)
}

/// Wakes what `push` is for, for `project`.
pub fn wake(signals: &WorkSignals, project: &str, push: &Push) {
    match push {
        Push::WorkAvailable => signals.wake_for(project, Wake::Work),
        Push::FactsChanged { kinds } => {
            if names(kinds, "expert_queue") {
                signals.wake_for(project, Wake::ExpertQueue);
            }
            if names(kinds, "deploy_requests") {
                signals.wake_for(project, Wake::DeployRequests);
            }
            if names(kinds, "intents") {
                signals.wake_for(project, Wake::Intents);
            }
        }
        Push::Reconnected => {
            for kind in [Wake::Work, Wake::ExpertQueue, Wake::DeployRequests, Wake::Intents] {
                signals.wake_for(project, kind);
            }
        }
        _ => {}
    }
}
