//! Writing a note (`releaseNotes.ts`'s `checkRange`, `generate` and `write`): checks the range a command asks for,
//! then turns a grouping into the note's Markdown, running the read-only `writer` session through
//! [`crate::engines`] only when there's something for the readers. A session that fails, or answers anything but
//! `ok`, still leaves a note: the plain list of what's new, with the reason in [`Written::error`]. Keeping the note
//! (the store's `release_note` row) and its gathering/writing states is the caller's.
use std::future::Future;
use std::path::Path;

use anyhow::{Result, bail};
use chrono::{DateTime, NaiveDate, SecondsFormat, Utc};

use crate::config::ProjectType;
use crate::engines::engine::{CursorOptions, EngineId, LocalTarget};
use crate::engines::{SessionResult, SessionSpec, SessionStart, describe_failure};

use super::group::{counts, facts, for_users};
use super::model::{Counts, Range, ReleaseNotesResult, Section, release_notes_result_schema};
use super::render::{build_prompt, empty_note, markdown};

/// `writerModel` (orchestratorHelpers.ts): the writer runs on the scouts' tier when that's the local model, else on
/// haiku.
pub fn writer_model(scout_tier: &str) -> &'static str {
    if scout_tier == "local" { "local" } else { "haiku" }
}

/// An ISO instant or a bare `YYYY-MM-DD` (midnight UTC, as `Date.parse` reads one).
fn parse_instant(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            Some(
                NaiveDate::parse_from_str(s, "%Y-%m-%d")
                    .ok()?
                    .and_hms_opt(0, 0, 0)?
                    .and_utc(),
            )
        })
}

/// `checkRange`: both ends are dates, From isn't after To, and neither is in the future. To is the end of its day,
/// so it may be up to two days ahead of `now` (a time zone either side). Answers both as UTC ISO instants.
pub fn check_range(from: &str, to: &str, now: DateTime<Utc>) -> Result<(String, String)> {
    let (Some(f), Some(t)) = (parse_instant(from), parse_instant(to)) else {
        bail!("from and to must be dates.");
    };
    if f > t {
        bail!("To can't be before From.");
    }
    if f > now || t > now + chrono::Duration::days(2) {
        bail!("The range can't be in the future.");
    }
    let iso = |d: DateTime<Utc>| d.to_rfc3339_opts(SecondsFormat::Millis, true);
    Ok((iso(f), iso(t)))
}

/// What a release-notes session runs with, besides the grouping and the range.
pub struct Writer<'a> {
    /// `prompts/release-notes.md`.
    pub template: &'a str,
    pub project_type: ProjectType,
    pub base_branch: &'a str,
    pub github_repo: &'a str,
    /// A checkout of the base branch, where the session may read the specs the facts name.
    pub cwd: &'a Path,
    /// `releaseFullEntries`.
    pub max_full_entries: usize,
    /// `budget.releaseNotesUsd`.
    pub budget_usd: f64,
    /// [`writer_model`], or `"local"` only while the local model is free.
    pub model: String,
    pub local: Option<LocalTarget>,
    /// [`crate::engines::engine_for`] with `Role::Writer`.
    pub engine: Option<EngineId>,
    pub cursor: Option<CursorOptions>,
    pub sandbox: bool,
}

/// The note [`write_note`] made.
#[derive(Debug, Clone)]
pub struct Written {
    pub markdown: String,
    /// The model the session ran on, or empty when none ran.
    pub model: String,
    pub cost_usd: f64,
    /// Why the note is the plain list rather than the model's words.
    pub error: Option<String>,
    pub counts: Counts,
}

/// The read-only, interactive-like-an-ask `writer` session for a grouping: nothing to resume, so it's ephemeral.
pub fn session_spec(w: &Writer<'_>, sections: &[Section], range: &Range) -> Result<SessionSpec> {
    let facts = facts(sections, w.max_full_entries);
    Ok(SessionSpec {
        engine: w.engine,
        model: w.model.clone(),
        prompt: build_prompt(w.template, w.project_type, w.base_branch, w.github_repo, range, &facts)?,
        schema: release_notes_result_schema(),
        cwd: w.cwd.to_path_buf(),
        max_turns: None,
        budget_usd: w.budget_usd,
        start: SessionStart::Ephemeral,
        readonly: true,
        cursor: w.cursor.clone(),
        local: w.local.clone(),
        sandbox: w.sandbox,
    })
}

/// `generate` after gathering, and `write`: an empty range gets [`empty_note`], one with only chores the plain list
/// (nothing for the readers, so nothing for the model to write), and anything else a session, run by `run` (the
/// caller's [`crate::engines::run_session`]).
pub async fn write_note<F, Fut>(
    sections: &[Section],
    total_prs: usize,
    range: &Range,
    w: &Writer<'_>,
    run: F,
) -> Written
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let counts = counts(sections, total_prs);
    let done = |res: Option<&ReleaseNotesResult>, model: String, cost_usd: f64, error: Option<String>| Written {
        markdown: markdown(sections, range, res),
        model,
        cost_usd,
        error: error.map(|e| format!("Claude {e}, so this is the plain list of what's new.")),
        counts,
    };
    if sections.is_empty() {
        return Written {
            markdown: empty_note(range),
            model: String::new(),
            cost_usd: 0.0,
            error: None,
            counts,
        };
    }
    if for_users(sections).is_empty() {
        return done(None, String::new(), 0.0, None);
    }
    let spec = match session_spec(w, sections, range) {
        Ok(spec) => spec,
        Err(e) => return done(None, String::new(), 0.0, Some(format!("couldn't write them ({e:#})"))),
    };
    let model = spec.model.clone();
    let r = match run(spec).await {
        Ok(r) => r,
        Err(e) => return done(None, model, 0.0, Some(format!("couldn't write them ({e:#})"))),
    };
    let cost_usd = r.cost_usd.unwrap_or(0.0);
    if !r.ok {
        return done(None, model, cost_usd, Some(describe_failure(&r, w.budget_usd)));
    }
    let res = r
        .structured
        .and_then(|v| serde_json::from_value::<ReleaseNotesResult>(v).ok());
    match res {
        Some(res) if res.status == "ok" => done(Some(&res), model, cost_usd, None),
        other => {
            let why = other.and_then(|r| r.error).filter(|e| !e.is_empty());
            let error = format!(
                "couldn't write them{}",
                why.map(|e| format!(": {e}")).unwrap_or_default()
            );
            done(None, model, cost_usd, Some(error))
        }
    }
}

#[cfg(test)]
#[path = "write_tests.rs"]
mod tests;
