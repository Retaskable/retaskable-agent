//! Triage's shared words (docs/specs/agent/06.9-triage-claude-looks-at-needs-you-first.md). The agent starts no
//! triager of its own: the store sends a `triage` job as a task, and [`crate::jobs::triage`] runs the read-only session
//! and answers its result, with the store deciding what the item becomes. What lives here is the vocabulary: the
//! decisions the buttons carry out (`continue`, `start_over`, `skip`, `human`, `person_steps`), the Retaskable bug a
//! look may name ([`RetaskableBug`], scrubbed and decided by [`bug_report::decide_bug_report`] before it goes to the
//! hub), the known bugs the hub listed, and [`schedule`] and [`record`], the facts about a stop the agent reports
//! (`claudeOnIt`).
pub mod bug_report;
pub mod record;
pub mod schedule;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What the triager is given about the item (06.9 bullet 1): gathered by the caller from the
/// item's own state, not read here.
#[derive(Debug, Clone)]
pub struct TriageContext {
    /// Why the item is `needs_human` right now.
    pub reason: String,
    /// The state it failed in (e.g. "reviewing", "merging") — not `needs_human` itself.
    pub failed_in: String,
    /// The recent timeline, newest first, each one line.
    pub timeline: Vec<String>,
    /// What the last check round reported, if one has run.
    pub last_check_result: Option<String>,
    pub pr_url: Option<String>,
    pub branch: Option<String>,
    pub has_pushed_branch: bool,
    pub has_session_to_continue: bool,
    /// How many automatic decisions this slice has had already, counted across start-overs
    /// (06.9: "at most 2 automatic decisions per slice").
    pub automatic_decisions: u32,
    /// Where this item's session transcripts are, for the triager to read if it wants to.
    pub transcripts_dir: Option<String>,
    /// Bugs already on Retaskable's board that the hub listed (`bug_reports`' `known`), so the
    /// triager can name one in `retaskableBug.sameAs` when this is the same bug worded
    /// differently (06.9 bullet 18).
    pub known_bugs: Vec<KnownBug>,
}

/// One bug already reported, as the hub's `bug_reports` message listed it.
#[derive(Debug, Clone)]
pub struct KnownBug {
    pub id: String,
    pub title: String,
}

/// `decision` (06.9 bullet 2): one of the five the orchestrator carries out the same way as the
/// buttons on the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageDecision {
    Continue,
    StartOver,
    Skip,
    Human,
    PersonSteps,
}

/// `suggest` (06.9's opening line): which button the app would offer first, when the triager
/// leaves the item for a person — only the three a person can actually press.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TriageButton {
    Continue,
    StartOver,
    Skip,
}

impl TriageButton {
    /// The decision this button carries out, the same as the triager's own `continue`/
    /// `start_over`/`skip` (06.9 bullet 3: "carried out like the triager's own decision").
    pub fn decision(self) -> TriageDecision {
        match self {
            TriageButton::Continue => TriageDecision::Continue,
            TriageButton::StartOver => TriageDecision::StartOver,
            TriageButton::Skip => TriageDecision::Skip,
        }
    }

    /// How this button reads as a verb, for the timeline line 06.9 gives: "Claude decided to …,
    /// checked with a second look: …".
    pub fn verb(self) -> &'static str {
        match self {
            TriageButton::Continue => "continue",
            TriageButton::StartOver => "start over",
            TriageButton::Skip => "skip",
        }
    }
}

/// `retaskableBug` (06.9 bullet 18): set only when what went wrong is Retaskable's own, never the
/// project's. [`bug_report::decide_bug_report`] scrubs it and decides whether to send it before it
/// ever leaves this computer.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RetaskableBug {
    pub title: String,
    pub details: String,
    /// The id of a [`KnownBug`] this is the same bug as, worded differently.
    #[serde(default, rename = "sameAs")]
    pub same_as: Option<String>,
}

/// The `retaskableBug` property of a result schema, shared with [`crate::looks`].
pub fn retaskable_bug_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "details": { "type": "string" },
            "sameAs": { "type": "string" }
        },
        "required": ["title", "details"]
    })
}
