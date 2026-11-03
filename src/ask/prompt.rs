//! Fills `prompts/assistant.md` (spec 06.6; `stateSummary`, `exchanges` and `audiencesText` in Retaskable.com's
//! `agent/src/assistant.ts`). The inputs are plain structs so the prompt doesn't depend on the store's types.
use anyhow::Result;

use crate::config::ProjectType;
use crate::prompt::{project_words, render_prompt};

/// How many exchanges the prompt shows.
const EXCHANGES_KEPT: usize = 5;

/// One message in the chat, as the prompt needs it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Exchange {
    pub id: String,
    pub text: String,
    pub answer: Option<String>,
    pub reply_to: Option<String>,
    /// `from: "retaskable"`: a message the agent wrote in code, never a person's question.
    pub from_retaskable: bool,
}

/// What an earlier action came to is not shown; only the words are.
#[derive(Debug, Clone, PartialEq)]
pub struct StateItem {
    pub id: String,
    pub title: String,
    pub state: String,
    pub backlog_status: String,
    pub lane: Option<u32>,
    pub change_url: Option<String>,
    pub needs_person: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateLane {
    pub lane: u32,
    pub item_id: Option<String>,
    pub running: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StateDraft {
    pub id: String,
    pub title: String,
    pub state: String,
}

/// The current state the prompt summarises (items, lanes, drafts, status, spend).
#[derive(Debug, Clone, PartialEq)]
pub struct StateSummary {
    /// The item or plan the message was asked on: it is listed first.
    pub about: Option<String>,
    pub items: Vec<StateItem>,
    pub lanes: Vec<StateLane>,
    pub drafts: Vec<StateDraft>,
    pub status: String,
    pub paused_reason: Option<String>,
    pub spent_today_usd: f64,
    pub daily_usd: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudienceLine {
    pub id: String,
    pub name: String,
    pub who: Option<String>,
}

/// Everything `build_prompt` fills in.
pub struct AssistantInput<'a> {
    pub project_type: ProjectType,
    pub base_branch: &'a str,
    pub github_repo: &'a str,
    /// The code host's words for a change (`{{pr}}`, `{{change}}`): "PR"/"pull request", "MR"/"merge request".
    pub pr: &'a str,
    pub change: &'a str,
    pub state: &'a StateSummary,
    pub audiences: &'a [AudienceLine],
    /// Every earlier ask, oldest first (cleared ones too), not including this one.
    pub asks: &'a [Exchange],
    pub reply_to: Option<&'a str>,
    pub question: &'a str,
}

/// The exchanges the prompt shows, oldest first: for a new ask the last five asks (cleared ones count); for a
/// reply the thread it continues (the ask it replies to and what that replied to, up to five), so asks that came
/// in between are skipped. Asks with no answer yet are left out, except Retaskable's own messages.
pub fn select_exchanges<'a>(asks: &'a [Exchange], reply_to: Option<&str>) -> Vec<&'a Exchange> {
    let shown = |a: &&Exchange| a.from_retaskable || a.answer.is_some();
    let mut list: Vec<&Exchange> = match reply_to {
        None => asks.iter().filter(shown).collect(),
        Some(first) => {
            let mut chain = Vec::new();
            let mut next = asks.iter().find(|a| a.id == first);
            while let Some(a) = next.filter(|_| chain.len() < asks.len()) {
                chain.push(a);
                next = a.reply_to.as_deref().and_then(|id| asks.iter().find(|x| x.id == id));
            }
            chain.reverse();
            chain.into_iter().filter(shown).collect()
        }
    };
    list.drain(..list.len().saturating_sub(EXCHANGES_KEPT));
    list
}

fn exchanges_text(list: &[&Exchange]) -> String {
    if list.is_empty() {
        return "(none)".to_string();
    }
    list.iter()
        .map(|a| {
            if a.from_retaskable {
                format!("Retaskable: {}", a.text)
            } else {
                format!("Human: {}\nYou: {}", a.text, a.answer.as_deref().unwrap_or(""))
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn audiences_text(audiences: &[AudienceLine]) -> String {
    audiences
        .iter()
        .map(|a| match a.who.as_deref().filter(|w| !w.is_empty()) {
            Some(who) => format!("- `{}`: {}. {who}", a.id, a.name),
            None => format!("- `{}`: {}", a.id, a.name),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn state_text(s: &StateSummary) -> String {
    let mut items: Vec<&StateItem> = s.items.iter().collect();
    // Stable: the `about` item moves to the front, the rest keep their order.
    items.sort_by_key(|i| s.about.as_deref() != Some(i.id.as_str()));
    let items = items
        .iter()
        .map(|i| {
            let mut line = format!("- {} {}: {} (backlog: {})", i.id, i.title, i.state, i.backlog_status);
            if let Some(l) = i.lane.filter(|l| *l > 0) {
                line.push_str(&format!(", lane {l}"));
            }
            if let Some(u) = &i.change_url {
                line.push_str(&format!(", {u}"));
            }
            if let Some(r) = &i.needs_person {
                line.push_str(&format!(", needs a human: {r}"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n");
    let lanes = s
        .lanes
        .iter()
        .map(|l| {
            let who = l.item_id.as_ref().map_or("free".to_string(), |i| format!("item {i}"));
            format!("- lane {}: {who}{}", l.lane, if l.running { " (running)" } else { "" })
        })
        .collect::<Vec<_>>()
        .join("\n");
    let drafts = s
        .drafts
        .iter()
        .map(|d| format!("- {} {}: {}", d.id, d.title, d.state))
        .collect::<Vec<_>>()
        .join("\n");
    let asked = match &s.about {
        Some(id) => format!("Where it was asked: on the page of item {id}"),
        None => "Where it was asked: the main chat".to_string(),
    };
    let paused = s.paused_reason.as_ref().map_or(String::new(), |r| format!(" ({r})"));
    [
        asked,
        format!(
            "Status: {}{paused}. Spent today: ${:.2} of ${}.",
            s.status, s.spent_today_usd, s.daily_usd
        ),
        format!("Items:\n{}", if items.is_empty() { "(none yet)" } else { &items }),
        format!("Lanes:\n{lanes}"),
        format!(
            "Spec drafts:\n{}",
            if drafts.is_empty() { "(none open)" } else { &drafts }
        ),
    ]
    .join("\n\n")
}

/// Fills the assistant's template.
pub fn build_prompt(template: &str, input: &AssistantInput) -> Result<String> {
    let words = project_words(input.project_type);
    render_prompt(
        template,
        &[
            ("projectKind", words.kind.to_string()),
            ("projectNoun", words.noun.to_string()),
            ("pr", input.pr.to_string()),
            ("change", input.change.to_string()),
            ("baseBranch", input.base_branch.to_string()),
            ("githubRepo", input.github_repo.to_string()),
            ("audiences", audiences_text(input.audiences)),
            ("state", state_text(input.state)),
            ("history", exchanges_text(&select_exchanges(input.asks, input.reply_to))),
            ("question", input.question.to_string()),
        ],
    )
}

#[cfg(test)]
mod tests;
