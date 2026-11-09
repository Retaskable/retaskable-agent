//! The vet-suggestions session (docs/specs/agent/06.7-chores-never-idle.md, "Vetting visitors' suggestions"):
//! one read-only session on the scouts' tier judges a batch of at most [`MAX_SUGGESTIONS_PER_RUN`] visitor
//! suggestions. Pure: the caller reads the specs index and the past verdicts, asks the hub, and sends what
//! [`plan`] returns. A session that fails yields no verdicts at all, never a partial batch.
use crate::text::is_blank;
use std::collections::HashSet;
use std::future::Future;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::backlog::suggestions::{Suggestion, Verdict};
use crate::config::ProjectType;
use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec, describe_failure};
use crate::ideas::{IdeaState, StoreIdeas};
use crate::prompt::{project_words, render_prompt};

/// The most suggestions one run judges; the rest wait for the next day.
pub const MAX_SUGGESTIONS_PER_RUN: usize = 20;
/// A suggestion's text is cut to this many characters before it reaches the prompt.
pub const SUGGESTION_TEXT_MAX: usize = 1000;
/// A verdict's `reason` is cut to this many characters.
pub const REASON_MAX: usize = 300;

/// The first [`MAX_SUGGESTIONS_PER_RUN`] of the hub's list (which answers oldest first).
pub fn take_batch(all: &[Suggestion]) -> &[Suggestion] {
    &all[..all.len().min(MAX_SUGGESTIONS_PER_RUN)]
}

/// A verdict this project gave earlier, with its reason.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PastVerdict {
    pub verdict: Verdict,
    pub reason: String,
}

/// What the state file keeps under `vet` (06.7): the day of the last run, its counts by verdict name, and the
/// last verdicts given (oldest first), so a restart keeps the history the prompt needs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct VetRecord {
    pub day: String,
    pub counts: std::collections::BTreeMap<String, usize>,
    pub recent: Vec<PastVerdict>,
}

/// A judged suggestion, cleaned.
#[derive(Debug, Clone, PartialEq)]
pub struct Judged {
    pub id: String,
    pub verdict: Verdict,
    pub reason: String,
    /// Only on a `duplicate`.
    pub matches: Option<String>,
}

/// What a vet session runs with.
pub struct Vetter<'a> {
    /// `prompts/vet-suggestions.md`.
    pub template: &'a str,
    pub project_type: ProjectType,
    /// `docs/specs/README.md`'s contents, read by the caller.
    pub specs_index: &'a str,
    pub ideas: &'a StoreIdeas,
    /// The last 30 verdicts given, newest first or last as the caller keeps them.
    pub past: &'a [PastVerdict],
    /// `Role::Scout`'s engine/model/budget bundle.
    pub session: EphemeralSessionConfig<'a>,
}

fn cut(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

fn titles(ideas: &StoreIdeas, state: IdeaState) -> String {
    let t: Vec<String> = ideas
        .ideas
        .iter()
        .filter(|i| i.state == state)
        .map(|i| format!("- {}", i.title))
        .collect();
    if t.is_empty() {
        "(none)".to_string()
    } else {
        t.join("\n")
    }
}

fn verdict_name(v: Verdict) -> &'static str {
    match v {
        Verdict::Promising => "promising",
        Verdict::Duplicate => "duplicate",
        Verdict::NotPromising => "not_promising",
        Verdict::Spam => "spam",
    }
}

/// Fills the template with the batch (each text cut to [`SUGGESTION_TEXT_MAX`]) and what the project knows.
pub fn build_prompt(v: &Vetter<'_>, batch: &[Suggestion]) -> Result<String> {
    let words = project_words(v.project_type);
    let suggestions = batch
        .iter()
        .map(|s| {
            format!(
                "- id: {}\n  came in: {}\n  text: {}",
                s.id,
                s.at,
                cut(&s.text, SUGGESTION_TEXT_MAX)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let past = if v.past.is_empty() {
        "(none)".to_string()
    } else {
        v.past
            .iter()
            .map(|p| format!("- {}: {}", verdict_name(p.verdict), p.reason))
            .collect::<Vec<_>>()
            .join("\n")
    };
    render_prompt(
        v.template,
        &[
            ("projectNoun", words.noun.to_string()),
            ("count", batch.len().to_string()),
            ("suggestions", suggestions),
            ("specsIndex", v.specs_index.to_string()),
            ("waiting", titles(v.ideas, IdeaState::Waiting)),
            ("accepted", titles(v.ideas, IdeaState::Accepted)),
            ("never", titles(v.ideas, IdeaState::Never)),
            ("pastVerdicts", past),
        ],
    )
}

/// For `SessionSpec.schema`.
pub fn result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "verdicts": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "verdict": { "type": "string", "enum": ["promising", "duplicate", "not_promising", "spam"] },
                        "reason": { "type": "string" },
                        "matches": { "type": "string" }
                    },
                    "required": ["id", "verdict", "reason"]
                }
            }
        },
        "required": ["verdicts"]
    })
}

#[derive(Debug, Deserialize)]
struct Answer {
    verdicts: Vec<Raw>,
}

#[derive(Debug, Deserialize)]
struct Raw {
    id: String,
    verdict: Verdict,
    reason: String,
    #[serde(default)]
    matches: Option<String>,
}

/// Drops verdicts for ids not in the batch or already given, keeps the session's order, cuts reasons to
/// [`REASON_MAX`] and keeps `matches` only on a `duplicate`.
fn clean(raw: Vec<Raw>, batch: &[Suggestion]) -> Vec<Judged> {
    let known: HashSet<&str> = batch.iter().map(|s| s.id.as_str()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    raw.into_iter()
        .filter(|r| known.contains(r.id.as_str()) && seen.insert(r.id.clone()))
        .map(|r| Judged {
            matches: if r.verdict == Verdict::Duplicate {
                r.matches.filter(|m| !is_blank(m))
            } else {
                None
            },
            reason: cut(r.reason.trim(), REASON_MAX).to_string(),
            id: r.id,
            verdict: r.verdict,
        })
        .collect()
}

/// The read-only, ephemeral session on the scouts' tier.
pub fn session_spec(v: &Vetter<'_>, batch: &[Suggestion]) -> Result<SessionSpec> {
    Ok(v.session.spec(build_prompt(v, batch)?, result_schema()))
}

/// What [`plan`] came back with.
#[derive(Debug, Clone)]
pub struct Planned {
    /// `None` when the run failed: send nothing.
    pub verdicts: Option<Vec<Judged>>,
    pub cost_usd: f64,
    pub note: Option<String>,
}

/// Runs the session through `run` and reads back its cleaned verdicts.
pub async fn plan<F, Fut>(v: &Vetter<'_>, batch: &[Suggestion], run: F) -> Planned
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let failed = |cost_usd: f64, note: String| Planned {
        verdicts: None,
        cost_usd,
        note: Some(note),
    };
    let spec = match session_spec(v, batch) {
        Ok(s) => s,
        Err(e) => return failed(0.0, format!("couldn't vet the suggestions ({e:#})")),
    };
    let r = match run(spec).await {
        Ok(r) => r,
        Err(e) => return failed(0.0, format!("couldn't vet the suggestions ({e:#})")),
    };
    let cost_usd = r.cost_usd.unwrap_or(0.0);
    if !r.ok {
        return failed(cost_usd, describe_failure(&r, v.session.budget_usd));
    }
    match r.structured.and_then(|s| serde_json::from_value::<Answer>(s).ok()) {
        Some(a) => Planned {
            verdicts: Some(clean(a.verdicts, batch)),
            cost_usd,
            note: None,
        },
        None => failed(cost_usd, "didn't answer with usable verdicts".to_string()),
    }
}

#[cfg(test)]
mod tests;
