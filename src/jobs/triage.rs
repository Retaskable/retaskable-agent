//! A `triage` task (`runTriage` in Retaskable.com's `agent/src/triageTask.ts`, jobs-on-server.md section 6.7): the
//! store sends what it knows of an item parked for help, and this runs the read-only session and answers the
//! structured result. It decides nothing: whether to look, the second look, the limits, the retries and what the
//! item becomes are the store's. `look: "check"` is the top tier's check of a clear suggestion; anything else is the
//! first look. A Retaskable bug the first look names is returned beside the answer for the caller to send, scrubbed
//! (bug-reports.md section 3.2, [`crate::triage::bug_report`]): the computer holds the consent and the known bugs.
//!
//! The prompts are `prompts/job-triage.md` and `prompts/job-triage-check.md`, the TypeScript agent's current ones
//! (host-neutral: `{{pr}}`, the item's own facts). [`crate::triage`]'s older prompts and `{{context}}` JSON are what
//! the claim-mode triager still uses; they are not these.
use std::future::Future;

use anyhow::Result;
use serde::Serialize;
use serde_json::{Value, json};

use super::{JobSpec, LimitHit, Outcome};
use crate::engines::{EphemeralSessionConfig, SessionResult, SessionSpec, describe_failure};
use crate::prompt::render_prompt;
use crate::triage::{KnownBug, RetaskableBug};

const TEMPLATE: &str = include_str!("../../prompts/job-triage.md");
const CHECK_TEMPLATE: &str = include_str!("../../prompts/job-triage-check.md");

/// The refusal when there is no checkout to look in.
pub const NO_CHECKOUT: &str = "This computer has no checkout of the project to look in.";

/// The first look's answer, shown to the check (`first`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FirstLook {
    pub reason: String,
    pub suggest: String,
    pub instructions: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TimelineLine {
    pub at: String,
    pub text: String,
}

/// One check result as the store sends it with the item (`CheckResult`).
#[derive(Debug, Clone, PartialEq)]
pub struct CheckLine {
    pub ok: bool,
    pub name: String,
    pub blocking: bool,
    pub preexisting: bool,
    pub duration_sec: String,
}

/// What `task.job.args` says of the item (`runTriage`'s `a`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TriageArgs {
    pub id: String,
    /// `look: "check"`: the top tier's check of a clear suggestion rather than the first look.
    pub check: bool,
    pub first: FirstLook,
    pub branch: Option<String>,
    pub pr_url: Option<String>,
    pub title: String,
    pub slice: i64,
    pub spec_refs: Vec<String>,
    pub failed_in: Option<String>,
    pub reason: Option<String>,
    pub triages: i64,
    pub max_triages: i64,
    pub timeline: Vec<TimelineLine>,
    pub checks: Vec<CheckLine>,
}

fn s(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("").to_string()
}

fn n(v: Option<&Value>, default: i64) -> i64 {
    v.and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
        .unwrap_or(default)
}

impl TriageArgs {
    /// Reads `job.args` as `runTriage` does: a missing or mistyped field is its default.
    pub fn from_job(job: &JobSpec) -> TriageArgs {
        let a = &job.args;
        let first = a.get("first");
        let nonempty = |k: &str| Some(s(a.get(k))).filter(|v| !v.is_empty());
        TriageArgs {
            id: s(a.get("id")),
            check: a.get("look").and_then(Value::as_str) == Some("check"),
            first: FirstLook {
                reason: s(first.and_then(|f| f.get("reason"))),
                suggest: s(first.and_then(|f| f.get("suggest"))),
                instructions: s(first.and_then(|f| f.get("instructions"))),
            },
            branch: nonempty("branch"),
            pr_url: nonempty("prUrl"),
            title: s(a.get("title")),
            slice: n(a.get("slice"), 0),
            spec_refs: a
                .get("specRefs")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            failed_in: nonempty("failedIn"),
            reason: nonempty("reason"),
            triages: n(a.get("triages"), 0),
            max_triages: n(a.get("maxTriages"), 2),
            timeline: a
                .get("timeline")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .filter_map(|l| {
                            Some(TimelineLine {
                                at: l.get("at").and_then(Value::as_str).unwrap_or("undefined").to_string(),
                                text: l.get("text")?.as_str()?.to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
            checks: a
                .get("checks")
                .and_then(Value::as_array)
                .map(|v| {
                    v.iter()
                        .map(|c| CheckLine {
                            ok: c.get("ok").and_then(Value::as_bool).unwrap_or(false),
                            name: s(c.get("name")),
                            blocking: c.get("blocking").and_then(Value::as_bool).unwrap_or(false),
                            preexisting: c.get("preexisting").and_then(Value::as_bool).unwrap_or(false),
                            duration_sec: c
                                .get("durationSec")
                                .filter(|d| d.is_number())
                                .map_or_else(|| "undefined".to_string(), Value::to_string),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        }
    }
}

/// How the project's code host names a change and how the session reads it (`hostVars`, codeHost/index.ts).
#[derive(Debug, Clone, PartialEq)]
pub struct HostWords {
    /// "pull request", "merge request", "change".
    pub change: &'static str,
    /// "PR", "merge request", "change".
    pub pr: &'static str,
    pub host_reads: String,
    pub host_edits: &'static str,
}

/// `hostVars`: `kind` is the config's `codeHost.kind` (missing is GitHub); `branches_here` whether a plain remote's
/// branches are kept on this computer.
pub fn host_words(kind: &str, branches_here: bool) -> HostWords {
    match kind {
        "" | "github" => HostWords {
            change: "pull request",
            pr: "PR",
            host_reads: "`gh pr view` and `gh pr checks` on the PR".to_string(),
            host_edits: ", no `gh pr` edits",
        },
        "gitlab" => HostWords {
            change: "merge request",
            pr: "merge request",
            host_reads: "`glab mr view` and `glab ci status` on the merge request".to_string(),
            host_edits: ", no `glab mr` edits",
        },
        "remote" => HostWords {
            change: "change",
            pr: "change",
            host_reads: format!(
                "the change is its branch, {}",
                if branches_here {
                    "kept on this computer"
                } else {
                    "pushed to origin"
                }
            ),
            host_edits: "",
        },
        _ => HostWords {
            change: "change",
            pr: "change",
            host_reads: "the change is its branch, kept on this computer".to_string(),
            host_edits: "",
        },
    }
}

/// The bugs in Retaskable already reported, as the triage prompt lists them (bug-reports.md section 3.3): one line
/// each, its id then its title, or "(none)" (`knownBugsList`).
pub fn known_bugs_list(known: &[KnownBug]) -> String {
    if known.is_empty() {
        return "(none)".to_string();
    }
    known
        .iter()
        .map(|b| format!("- {}: {}", b.id, crate::text::squish(&b.title)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `checksSummary`: each check as "✓ name", "(non-blocking)", "(fails on main too, not this <pr>'s fault)", and its
/// seconds, one per line.
pub fn checks_summary(checks: &[CheckLine], pr: &str) -> String {
    checks
        .iter()
        .map(|c| {
            format!(
                "{} {}{}{} {}s",
                if c.ok { "✓" } else { "✗" },
                c.name,
                if c.blocking { "" } else { " (non-blocking)" },
                if c.preexisting {
                    format!(" (fails on main too, not this {pr}'s fault)")
                } else {
                    String::new()
                },
                c.duration_sec
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What this computer knows about the project and itself that the item's args don't.
pub struct TriageEnv<'a> {
    /// The project has a checkout here (`cfg`).
    pub has_checkout: bool,
    pub base_branch: &'a str,
    /// `codeHost.kind`, and whether a plain remote's branches are kept here.
    pub code_host_kind: &'a str,
    pub branches_here: bool,
    /// Where the item's session transcripts are (`runDir(id, item)`).
    pub runs_dir: &'a str,
    /// The bugs the hub listed as already reported (`pool.knownBugs`).
    pub known_bugs: &'a [KnownBug],
    /// The read-only session's engine, model (the top tier for a check, the chore tier for the first look), budget
    /// (`budget.reviewerUsd`), checkout (the planner's, the base branch) and sandbox.
    pub session: &'a EphemeralSessionConfig<'a>,
    /// When a usage limit lifts, epoch milliseconds (`limitResume`): the 5-hour reset, never sooner than 10 minutes.
    pub limit_resume_ms: &'a dyn Fn() -> i64,
}

/// `{{pr}}` and `{{change}}` inside a variable's own text, filled in from the same variables (`hostWords`).
fn host_text(v: &str, w: &HostWords) -> String {
    v.replace("{{pr}}", w.pr).replace("{{change}}", w.change)
}

/// The prompt for the look the args name. `Err` names a variable the template needs and wasn't given.
pub fn triage_prompt(a: &TriageArgs, env: &TriageEnv<'_>) -> Result<String> {
    let w = host_words(env.code_host_kind, env.branches_here);
    let work = if a.pr_url.is_some() {
        "Its work is on the {{pr}}'s branch.".to_string()
    } else if let Some(b) = &a.branch {
        format!("It has no {{{{pr}}}} yet; its work so far is pushed to `{b}`, and Start again begins a fresh session.")
    } else {
        "It has no {{pr}} and no work to keep.".to_string()
    };
    let or = |v: &Option<String>, d: &str| v.clone().unwrap_or_else(|| d.to_string());
    let timeline = a
        .timeline
        .iter()
        .map(|t| format!("- {} {}", t.at, t.text))
        .collect::<Vec<_>>()
        .join("\n");
    let vars: Vec<(&str, String)> = vec![
        ("pr", w.pr.to_string()),
        ("change", w.change.to_string()),
        ("hostReads", w.host_reads.clone()),
        ("hostEdits", w.host_edits.to_string()),
        ("itemId", a.id.clone()),
        ("title", a.title.clone()),
        ("slice", a.slice.to_string()),
        (
            "specRefs",
            if a.spec_refs.is_empty() {
                "(none)".to_string()
            } else {
                a.spec_refs.join(", ")
            },
        ),
        // The store keeps the backlog: no file in the repo holds it (`fileVars().backlogFileLine`).
        ("backlogFileLine", String::new()),
        ("baseBranch", env.base_branch.to_string()),
        ("branch", or(&a.branch, "(none)")),
        ("prUrl", or(&a.pr_url, "(none)")),
        ("work", work),
        ("failedIn", or(&a.failed_in, "unknown")),
        ("reason", or(&a.reason, "(no reason recorded)")),
        ("triages", a.triages.to_string()),
        ("maxTriages", a.max_triages.to_string()),
        (
            "timeline",
            if timeline.is_empty() {
                "(none)".to_string()
            } else {
                timeline
            },
        ),
        (
            "checks",
            if a.checks.is_empty() {
                "(none)".to_string()
            } else {
                checks_summary(&a.checks, "{{pr}}")
            },
        ),
        ("where", format!("a checkout of the base branch `{}`", env.base_branch)),
        ("runs", env.runs_dir.to_string()),
        ("knownBugs", known_bugs_list(env.known_bugs)),
        ("firstReason", a.first.reason.clone()),
        ("suggest", a.first.suggest.replacen('_', " ", 1)),
        (
            "firstInstructions",
            if a.first.instructions.is_empty() {
                "(none)".to_string()
            } else {
                a.first.instructions.clone()
            },
        ),
    ];
    let vars: Vec<(&str, String)> = vars.into_iter().map(|(k, v)| (k, host_text(&v, &w))).collect();
    let template = if a.check { CHECK_TEMPLATE } else { TEMPLATE };
    render_prompt(&template.replace("\r\n", "\n"), &vars)
}

/// `TRIAGE_SCHEMA`, for `SessionSpec.schema`.
pub fn triage_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "decision": { "enum": ["continue", "start_over", "skip", "human", "person_steps"] },
            "personSteps": { "type": "array", "items": { "type": "string" } },
            "reason": { "type": "string" },
            "instructions": { "type": "string" },
            "suggest": { "enum": ["continue", "start_over", "skip"] },
            "clear": { "type": "boolean" },
            "retaskableBug": crate::triage::retaskable_bug_schema()
        },
        "required": ["decision", "reason", "instructions", "suggest", "clear"]
    })
}

/// `TRIAGE_CHECK_SCHEMA`.
pub fn check_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "agree": { "type": "boolean" },
            "reason": { "type": "string" },
            "instructions": { "type": "string" }
        },
        "required": ["agree", "reason", "instructions"]
    })
}

/// What a triage task answers: `{ triage }` for the first look, `{ check }` for the check.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TriageAnswer {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub triage: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub check: Option<Value>,
}

/// A Retaskable bug the first look named, with what the report needs of the item (`reportLookBug`'s `about`): the
/// caller sends it scrubbed ([`crate::triage::bug_report::decide_bug_report`]), whatever the look decided.
#[derive(Debug, Clone, PartialEq)]
pub struct BugToReport {
    pub id: String,
    pub branch: Option<String>,
    /// The item's last failure's reason (`a.reason`).
    pub why: String,
    pub bug: RetaskableBug,
}

/// A triage task's end and the bug to send beside it.
#[derive(Debug)]
pub struct TriageRun {
    pub outcome: Outcome<TriageAnswer>,
    pub bug: Option<BugToReport>,
}

fn ended(outcome: Outcome<TriageAnswer>) -> TriageRun {
    TriageRun { outcome, bug: None }
}

/// The `triage` job. `run` is the caller's [`crate::engines::run_session`]. Never decides anything.
pub async fn triage_job<F, Fut>(job: &JobSpec, env: &TriageEnv<'_>, run: F) -> TriageRun
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    if !env.has_checkout {
        return ended(Outcome::refused(NO_CHECKOUT));
    }
    let a = TriageArgs::from_job(job);
    let prompt = match triage_prompt(&a, env) {
        Ok(p) => p,
        Err(e) => return ended(Outcome::failed(format!("it couldn't look into it ({e:#})"))),
    };
    let schema = if a.check { check_schema() } else { triage_schema() };
    let r = match run(env.session.spec(prompt, schema)).await {
        Ok(r) => r,
        Err(e) => return ended(Outcome::failed(format!("it couldn't look into it ({e:#})"))),
    };
    if !r.ok {
        return ended(Outcome::Failed {
            note: format!("it {}", describe_failure(&r, env.session.budget_usd)),
            no_retry: r.subtype == "error_max_budget_usd",
            limit_hit: r.limit_hit.then(|| LimitHit {
                engine: env.session.engine.map(|e| e.to_string()),
                until: (env.limit_resume_ms)(),
            }),
        });
    }
    let structured = r.structured.unwrap_or(Value::Null);
    if a.check {
        return ended(Outcome::done(TriageAnswer {
            triage: None,
            check: Some(structured),
        }));
    }
    let bug = structured
        .get("retaskableBug")
        .and_then(|b| serde_json::from_value::<RetaskableBug>(b.clone()).ok())
        .map(|bug| BugToReport {
            id: a.id.clone(),
            branch: a.branch.clone(),
            why: a.reason.clone().unwrap_or_default(),
            bug,
        });
    TriageRun {
        outcome: Outcome::done(TriageAnswer {
            triage: Some(structured),
            check: None,
        }),
        bug,
    }
}

#[cfg(test)]
mod tests;
