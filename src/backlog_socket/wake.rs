//! What a push wakes. Nothing is run from a push: it ends a loop's wait so the loop reads the store now instead of at
//! its next beat or poll (the store's reply, as ever, is what it acts on).
//!
//! `storeLink.ts`'s `pushed` does more (settings, pause, try-outs, ideas, drafts, facts, the intent puller); this agent
//! wakes only the loops that poll slower than a person waits:
//! - `work_available`: the run loop asks for work again, and, with a `watch` run live, beats now (`driver/runs.rs`): a passed
//!   `ci_finish` makes the store settle the piece itself and push this, and the beat is how the watch hears its `stop`
//!   instead of waiting out the minute (or, with no head recorded, the CI grace).
//! - `facts_changed` naming `expert_queue` (an edit or a cancel of a queued message on the site, or another computer
//!   taking one), `deploy_requests` or `intents` (the site wrote a row), or naming nothing (any may have changed): the
//!   expert sessions read their queue back, the deploy loop lists its requests, the intent puller of Add spec, Build this
//!   and the reads lists its rows (a read row times out after 30 seconds unclaimed, so a minute's poll is too slow).
//! - `reconnected` (the socket was down, so a push may have been missed): all four, and the watches' reads.
//!
//! - `tryout_problem`: kept for the project's loop, which takes the problem as the builder (`driver/reopening.rs`).
//!
//! - `drafts_changed` and `draft_answer`: the loop's draft step runs now (it claims a queued draft, or reads the answer);
//!   `drafts_changed` also has the plans waiting for approval looked at for a pull request merged by hand.
//!
//! - `changed`: the loop reads the items it has parked for an answer, for a person's answer kept on one.
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
        Push::WorkAvailable => {
            signals.wake_for(project, Wake::Work);
            signals.wake_ci(project);
        }
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
        // The builder takes it (`tryout_taken`) and the item reopens, within the 2 minutes the store gives before it makes a fix item.
        Push::TryoutProblem { key, notes } => {
            signals.problem(project, key, notes);
            signals.wake_for(project, Wake::Work);
        }
        Push::Reconnected => {
            for kind in [
                Wake::Work,
                Wake::ExpertQueue,
                Wake::DeployRequests,
                Wake::Intents,
                Wake::Changed,
            ] {
                signals.wake_for(project, kind);
            }
            signals.wake_ci(project);
        }
        // A draft was queued or answered: the loop wakes so its draft step claims it now, not at the next beat. The step
        // asks the store (`draft_claim_next`); nothing is decided from the push.
        // A changed draft may be a plan a person merged by hand: its project looks at its waiting plans too (`driver/land.rs`).
        Push::DraftsChanged { .. } => signals.wake_for(project, Wake::Drafts),
        Push::DraftAnswer { .. } => signals.kick(),
        // An item changed: a person may have answered the questions an item of this project is parked for, so the loop
        // reads the items it waits on for an answer now (`driver/site_answers.rs`). Nothing is decided from the push.
        Push::Changed { .. } => signals.wake_for(project, Wake::Changed),
        _ => {}
    }
}
