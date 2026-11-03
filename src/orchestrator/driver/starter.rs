//! What a project's loop asks of the computer to start work: the request types and the [`SessionStarter`] that runs
//! the sessions and steps.
use futures_util::future::BoxFuture;
use serde_json::Value;

use crate::backlog::Item;
use crate::engines::EngineId;
use crate::item_lifecycle::ItemRun;
use crate::orchestrator::steps::StepRequest;

/// What a started implementer needs: the item, the lane it holds and its lease token (the TS agent's
/// `hosted.lease`, which every later store write about the item carries).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartRequest {
    pub project: String,
    pub item_id: String,
    pub lane: u32,
    pub token: String,
    /// The claimed item as the store handed it out (title, slice, remaining, spec refs).
    pub item: Item,
    /// The session's id, saved in the state file before it starts: the one `--session-id` names for a new
    /// session, or the saved one `--resume` continues.
    pub session_id: String,
    /// The engine the session runs on: the one it started on when `resume`, else the lane's.
    pub engine: EngineId,
    /// Whether to resume `session_id` (a restart cut it off) rather than start it.
    pub resume: bool,
    /// Where the session reports how it ended; the loop reads it on its next wake.
    pub outcomes: super::OutcomeSender,
    /// The project's latest `state.usage` reading (section 7.1), for picking the implementer's
    /// model tier. `None` when there's no reading yet, which is treated as a full window.
    pub usage: Option<Value>,
    /// `None` starts the session fresh in a prepared lane, as a newly claimed item does. `Some` restarts an
    /// implementer in its existing lane after it ran out of turns or had grown too large (06.2-implementer.md
    /// "at most maxTurns turns"): the lane is reused as-is, keeping any uncommitted work, and this text is the
    /// session's prompt verbatim.
    pub follow_up: Option<String>,
    /// Every item the store listed, for a fresh session's **Items not built yet** list (and the later
    /// check of its `waitsOn` against it).
    pub items: Vec<Item>,
    /// How many times a person started this slice over: the branch is `…-s<slice>-r<attempt>` above 0.
    pub attempt: u32,
    /// A Start over's notes, for the fresh session's first prompt as notes from the last attempt.
    pub notes: Option<String>,
    /// The engine that takes over from `engine` if it is at its usage limit when the code gates send findings
    /// back (engines/03 section 3.5): the session starts fresh on it. `None` when `engine` isn't limited or
    /// nothing else can take it.
    pub takeover: Option<EngineId>,
}

/// What [`SessionStarter::abandon`] lets go of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Abandon {
    pub project: String,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
}

/// How `ProjectLoop::launch` starts an implementer session.
pub(super) enum Launch {
    /// A newly claimed item: a fresh session id in a freshly prepared lane.
    Fresh,
    /// A session a restart of this computer cut off: resumes the saved session id in its existing lane.
    Resume(String),
    /// A session that ran out of turns or had grown too large: a fresh session id, reusing the existing lane
    /// as-is, with this text as the picking-up prompt.
    Continuing(String),
    /// [`Continuing`](Self::Continuing) on this engine, not the lane's: the session's own engine is at its usage
    /// limit and this one takes over.
    ContinuingOn(String, EngineId),
    /// A person's Start over: a fresh session in a freshly prepared lane on the new attempt's branch, with these
    /// notes (if any) in its first prompt.
    StartOver(Option<String>),
}

/// Runs the sessions and steps for the work the loop schedules. Built in a later piece (implementer-session and
/// the review stages); until one is given to the [`super::Machine`], `claim` mode claims nothing, so no claimed item is
/// left with nothing working on it.
pub trait SessionStarter: Send + Sync {
    /// Called once the item is `implementing` and its lane held. Must not block: start the session and return;
    /// it reports its end through `request.outcomes`. An `Err` (the session was refused or couldn't start)
    /// gives the lane and the claim straight back.
    fn start(&self, request: StartRequest) -> Result<(), String>;
    /// Sessions running now, on every project: the pseudocode's `activeSessions`, counted against `concurrency`.
    fn active(&self) -> u32;
    /// Start over's first step: closes the item's old PR (`gh pr close`), or drops its old branch when there is
    /// no PR. A slice that was merged is never touched. An `Err` stops the Start over, leaving the item as it is.
    fn abandon(&self, _old: Abandon) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
    /// The item's next step for its state (`steps::has_step`, except `implementing`, whose step is its session),
    /// run as its own task: the run as the step left it. `None` when no step is built for that state yet: the item
    /// waits where it is, and the loop asks again on its next wake.
    /// The loop asks again only once it ends. A step that leaves the item in a state that runs nothing in a lane
    /// (`steps::needs_lane`) has pushed its work first: the loop then frees the lane.
    fn advance(&self, request: StepRequest) -> Option<BoxFuture<'static, ItemRun>>;
    /// Starts a read-only triager session on a stopped item (06.9). Must not block; it reports its end through
    /// `request.outcomes` as [`SessionResult::Triage`](super::SessionResult::Triage). An `Err` counts as a failed look.
    fn triage(&self, _request: super::TriageRequest) -> Result<(), String> {
        Err("this computer has no triager".into())
    }
    /// Starts the top-tier second look at a triager's `human` answer with a suggested button (06.9 bullet 3). Must not
    /// block; it reports its end through `request.triage.outcomes` as
    /// [`SessionResult::SecondLook`](super::SessionResult::SecondLook). An `Err` counts as a failed second look.
    fn second_look(&self, _request: super::SecondLookRequest) -> Result<(), String> {
        Err("this computer has no second look".into())
    }
    /// Runs `command` in `dir` as a Play of lane `lane` (visible, unsandboxed, `RETASKABLE_LANE=<lane>`).
    fn play_lane(&self, dir: &std::path::Path, command: &str, lane: u32) -> Result<Box<dyn LanePlay>, String> {
        crate::play::start_in_lane(dir, command, lane).map(|g| Box::new(g) as Box<dyn LanePlay>)
    }
}

/// A game running in an item's lane.
pub trait LanePlay: Send {
    /// Whether the game has closed.
    fn finished(&self) -> bool;
    /// Ends the game; it is [`finished`](Self::finished) once it is gone.
    fn stop(&self);
}

impl LanePlay for crate::play::LaneGame {
    fn finished(&self) -> bool {
        self.finished()
    }
    fn stop(&self) {
        self.stop()
    }
}
