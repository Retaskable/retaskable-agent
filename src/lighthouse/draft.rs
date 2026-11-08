//! Which findings are new and the `[Lighthouse]` draft that writes them up (daily-lighthouse.md sections 2 and 3;
//! `isNewFinding`, `newFindings` and `lighthouseRequest` in Retaskable.com's `agent/src/lighthouse.ts`). No I/O.

use std::collections::{HashMap, HashSet};

use anyhow::Result;

use super::{Finding, Scores, Summary};
use crate::backlog::asks::QueuedDraft;
use crate::prompt::render_prompt;

/// The tag a Lighthouse draft's request starts with; the drafter runs such a draft on the chore tier.
pub const TAG: &str = "[Lighthouse]";
/// A discarded draft's findings stay quiet this long, counted from its last timeline entry.
const DISCARD_QUIET_MS: i64 = 30 * 24 * 60 * 60 * 1000;
const TEMPLATE: &str = include_str!("../../prompts/lighthouse.md");

/// What the store says about the draft a known key went into.
#[derive(Debug, Clone, PartialEq)]
pub struct DraftState {
    /// `open`, `merged`, `discarded` or `failed`.
    pub status: String,
    /// Milliseconds since 1970 of the draft's last timeline entry, in the store's time.
    pub last_entry_ms: i64,
}

/// The known keys, each with its draft's state; `None` is a draft the store no longer has.
pub type Known = HashMap<String, Option<DraftState>>;

/// A key is quiet while its draft is open or merged, or discarded under 30 days ago (`now_ms` is the store's
/// time). An unknown key, a failed draft and a missing draft are new.
pub fn is_new_finding(key: &str, known: &Known, now_ms: i64) -> bool {
    match known.get(key) {
        Some(Some(d)) => match d.status.as_str() {
            "open" | "merged" => false,
            "discarded" => now_ms - d.last_entry_ms >= DISCARD_QUIET_MS,
            _ => true,
        },
        _ => true,
    }
}

/// The run's findings that are new, each key once.
pub fn new_findings<'a>(summaries: &'a [Summary], known: &Known, now_ms: i64) -> Vec<(&'a Summary, &'a Finding)> {
    let mut seen = HashSet::new();
    summaries
        .iter()
        .flat_map(|s| s.findings.iter().map(move |f| (s, f)))
        .filter(|(_, f)| is_new_finding(&f.key, known, now_ms) && seen.insert(f.key.as_str()))
        .collect()
}

fn score(label: &str, v: Option<u32>) -> Option<String> {
    v.map(|v| format!("{label} {v}"))
}

fn scores_text(s: &Scores) -> String {
    [
        score("performance", s.performance),
        score("accessibility", s.accessibility),
        score("best practices", s.best_practices),
        score("SEO", s.seo),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(", ")
}

fn finding_text(f: &Finding) -> String {
    let mut out = format!("- `{}`: {}", f.key, f.title);
    if !f.display_value.is_empty() {
        out.push_str(&format!(" ({})", f.display_value));
    }
    for item in &f.items {
        out.push_str(&format!("\n  - {item}"));
    }
    out
}

/// `lighthouseRequest`: the draft to queue for the day's new findings, or `None` when there are none. `url` is the
/// staging url and `specs_index` the repo's specs index.
pub fn lighthouse_request(
    url: &str,
    summaries: &[Summary],
    new: &[(&Summary, &Finding)],
    specs_index: &str,
) -> Result<Option<QueuedDraft>> {
    if new.is_empty() {
        return Ok(None);
    }
    let pages = summaries
        .iter()
        .map(|s| format!("- `{}` ({}): {}", s.page, s.preset, scores_text(&s.scores)))
        .collect::<Vec<_>>()
        .join("\n");
    let findings = new.iter().map(|(_, f)| finding_text(f)).collect::<Vec<_>>().join("\n");
    let request = render_prompt(
        TEMPLATE,
        &[
            ("tag", TAG.to_string()),
            ("url", url.to_string()),
            ("pages", pages),
            ("findings", findings),
            ("specsIndex", specs_index.to_string()),
        ],
    )?;
    let keys: Vec<&str> = new.iter().map(|(_, f)| f.key.as_str()).collect();
    Ok(Some(QueuedDraft {
        request,
        intake: Some(serde_json::json!({ "note": format!("Covers finding keys: {}", keys.join("; ")), "keys": keys })),
        ..QueuedDraft::default()
    }))
}
