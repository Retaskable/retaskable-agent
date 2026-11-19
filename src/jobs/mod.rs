//! The jobs a store task's `job` names (Retaskable.com's `agent/src/jobTasks.ts`, docs/specs/jobs-on-server.md
//! sections 3, 4 and 13): a fixed command (Lighthouse, the security probes, the accessibility sweep, vetting
//! suggestions, deprecated specs, triage, release notes) that this computer runs and answers with its raw
//! result. It decides nothing: no day, no lock, no schedule, no draft. The store writes the fact and works out
//! what is new. Refusals are the computer's own, in words: no Chrome, no staging url, a url that isn't the
//! project's staging. Never a secret in a result: the probes' test token is read at the run and stays in the
//! probes.
//!
//! Library code only: nothing here is wired to the driver. Each kind has one entry point taking a plain input
//! struct (what `task.job` carries, plus what the computer's own settings say) and its own dependency trait
//! (fakes in the tests), and answering an [`Outcome`] of that kind's result struct. A caller turns one into the
//! body of `task_end` with [`Outcome::into_job`] and [`end_body`].
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use serde_json::{Map, Value, json};

pub mod a11y;
pub mod a11y_browser;
pub mod a11y_judge;
pub mod a11y_sweep;
pub mod change_actions;
pub mod deprecations;
pub mod host;
pub mod inputs;
pub mod lighthouse;
pub mod lighthouse_reports;
pub mod lighthouse_summary;
pub mod playtest_group;
pub mod probes;
pub mod probes_checks;
mod probes_tls;
pub mod release;
pub mod run;
pub mod scan;
pub mod store;
pub mod triage;
pub mod vet;
pub mod vet_wire;

/// The kinds a task's `job` may carry (`JOB_KINDS`). A computer refuses one it doesn't know.
pub const JOB_KINDS: [&str; 14] = [
    "lighthouse",
    "probes",
    "a11y",
    "vet",
    "deprecations",
    "triage",
    "release_notes",
    "draft",
    "smoke",
    "main",
    "issues",
    "issues_finish",
    "change_actions",
    "playtest_group",
];

/// The kinds this computer runs here (`draft` is the drafter's own task and `smoke` has its own runner, `smoke_task.rs`:
/// neither is a job runner).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Lighthouse,
    Probes,
    A11y,
    Vet,
    Deprecations,
    Triage,
    ReleaseNotes,
    /// The store's look at the base branch (`main_look.rs`): no session, no slot.
    Main,
    /// Reads the project's issue tracker (`issues/import.rs`): no session, no slot.
    Issues,
    /// Comments on and closes the issues of merged plans (`issues/finish.rs`): no session, no slot.
    IssuesFinish,
    /// Closes or re-runs CI of the agent's own changes, as the store asks (`change_actions.rs`): no session, no slot.
    ChangeActions,
    /// Groups the waiting try-outs into themed sittings (`playtest_group.rs`): one read-only planner session, no lane.
    PlaytestGroup,
}

impl JobKind {
    pub fn parse(kind: &str) -> Option<JobKind> {
        Some(match kind {
            "lighthouse" => JobKind::Lighthouse,
            "probes" => JobKind::Probes,
            "a11y" => JobKind::A11y,
            "vet" => JobKind::Vet,
            "deprecations" => JobKind::Deprecations,
            "triage" => JobKind::Triage,
            "release_notes" => JobKind::ReleaseNotes,
            "main" => JobKind::Main,
            "issues" => JobKind::Issues,
            "issues_finish" => JobKind::IssuesFinish,
            "change_actions" => JobKind::ChangeActions,
            "playtest_group" => JobKind::PlaytestGroup,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            JobKind::Lighthouse => "lighthouse",
            JobKind::Probes => "probes",
            JobKind::A11y => "a11y",
            JobKind::Vet => "vet",
            JobKind::Deprecations => "deprecations",
            JobKind::Triage => "triage",
            JobKind::ReleaseNotes => "release_notes",
            JobKind::Main => "main",
            JobKind::Issues => "issues",
            JobKind::IssuesFinish => "issues_finish",
            JobKind::ChangeActions => "change_actions",
            JobKind::PlaytestGroup => "playtest_group",
        }
    }

    /// The jobs that run a session, so take one of the computer's slots (`SESSION_JOBS`, plus the drafter's):
    /// the rest run in no lane.
    pub fn runs_session(self) -> bool {
        matches!(
            self,
            JobKind::Vet | JobKind::Triage | JobKind::ReleaseNotes | JobKind::PlaytestGroup
        )
    }

    /// The jobs that only read or act with `git`, `gh` or a tracker's API: they take no slot and no lane, so they start
    /// whatever else holds this computer back (the store sends them on its own cadence).
    pub fn is_light(self) -> bool {
        matches!(
            self,
            JobKind::Main | JobKind::Issues | JobKind::IssuesFinish | JobKind::ChangeActions
        )
    }
}

/// The fixed command a task names (`JobSpec`): its `kind`, the project's `day` it is for, and the settings it needs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobSpec {
    pub kind: String,
    /// The project's `yyyy-mm-dd`, decided by the store.
    pub day: String,
    pub args: Map<String, Value>,
}

impl JobSpec {
    /// Reads `task.job` as the store sends it. `None` when it has no `kind`.
    pub fn from_json(v: &Value) -> Option<JobSpec> {
        Some(JobSpec {
            kind: v.get("kind")?.as_str()?.to_string(),
            day: v.get("day").and_then(Value::as_str).unwrap_or("").to_string(),
            args: v.get("args").and_then(Value::as_object).cloned().unwrap_or_default(),
        })
    }

    /// A string argument (`job.args?.url`).
    pub fn arg_str(&self, key: &str) -> Option<&str> {
        self.args.get(key).and_then(Value::as_str)
    }
}

/// What this computer says it has (`health`'s `can`): facts only, never a key. The order is `CAN`'s (shared/jobs.ts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Can {
    Chrome,
    Repo,
    Engine,
    ScanToken,
    /// The project is on GitHub, so this computer's own `gh` reads and writes its issues.
    IssuesGithub,
    /// This computer holds a Linear key (never sent).
    IssuesLinear,
    /// This computer holds a Jira key (never sent).
    IssuesJira,
    /// The project is on GitHub with a repo named, so this computer's own `gh` closes and re-runs the agent's changes.
    Changes,
}

impl Can {
    pub fn name(self) -> &'static str {
        match self {
            Can::Chrome => "chrome",
            Can::Repo => "repo",
            Can::Engine => "engine",
            Can::ScanToken => "scanToken",
            Can::IssuesGithub => "issues:github",
            Can::IssuesLinear => "issues:linear",
            Can::IssuesJira => "issues:jira",
            Can::Changes => "changes",
        }
    }
}

/// Chrome found missing since the agent started, for one project: `can` stops saying `chrome`, so no Chrome job is
/// sent until a restart (`noChrome`).
#[derive(Debug, Default)]
pub struct ChromeFlag(AtomicBool);

impl ChromeFlag {
    pub fn mark_missing(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn missing(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// What this computer reports it `can` do for a project (`jobCan`), in `CAN`'s order. `has_checkout` is a project
/// config being there (`repo` and `engine`); `probes_token` whether the probes' target is on, can run and has a
/// staging test token saved.
pub fn job_can(has_checkout: bool, chrome: &ChromeFlag, probes_token: bool) -> Vec<Can> {
    let mut has = Vec::new();
    if !chrome.missing() {
        has.push(Can::Chrome);
    }
    if has_checkout {
        has.push(Can::Repo);
        has.push(Can::Engine);
    }
    if probes_token {
        has.push(Can::ScanToken);
    }
    let order = [Can::Chrome, Can::Repo, Can::Engine, Can::ScanToken];
    order.into_iter().filter(|c| has.contains(c)).collect()
}

/// When a usage limit lifts, as a task's `limitHit` says it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LimitHit {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Epoch milliseconds.
    pub until: i64,
}

/// A job's end (`JobOutcome`), its result typed per kind: the result, a refusal in words (this computer cannot run
/// it), or a failure's words.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome<R> {
    Done {
        result: R,
        /// An old agent's own record of the findings it wrote up, sent for the store to take once
        /// (jobs-on-server.md section 8); empty when it has none.
        known: BTreeMap<String, String>,
    },
    Refused(String),
    Failed {
        note: String,
        no_retry: bool,
        limit_hit: Option<LimitHit>,
    },
}

impl<R> Outcome<R> {
    pub fn done(result: R) -> Self {
        Outcome::Done {
            result,
            known: BTreeMap::new(),
        }
    }

    pub fn refused(why: impl Into<String>) -> Self {
        Outcome::Refused(why.into())
    }

    pub fn failed(note: impl Into<String>) -> Self {
        Outcome::Failed {
            note: note.into(),
            no_retry: false,
            limit_hit: None,
        }
    }
}

impl<R: Serialize> Outcome<R> {
    /// The untyped end, for `task_end`.
    pub fn into_job(self) -> JobOutcome {
        match self {
            Outcome::Done { result, known } => JobOutcome::Done {
                result: serde_json::to_value(result).unwrap_or(Value::Null),
                known,
            },
            Outcome::Refused(why) => JobOutcome::Refused(why),
            Outcome::Failed {
                note,
                no_retry,
                limit_hit,
            } => JobOutcome::Failed {
                note,
                no_retry,
                limit_hit,
            },
        }
    }
}

/// [`Outcome`] with its result as JSON.
pub type JobOutcome = Outcome<Value>;

/// The body of `task_end` for an outcome (`endBody`).
pub fn end_body(out: &JobOutcome) -> Value {
    match out {
        Outcome::Done { result, known } => {
            let mut body = json!({ "ok": true, "result": result });
            if !known.is_empty() {
                body["known"] = json!(known);
            }
            body
        }
        Outcome::Refused(why) => json!({ "ok": false, "refused": { "why": why } }),
        Outcome::Failed {
            note,
            no_retry,
            limit_hit,
        } => {
            let mut body = json!({ "ok": false, "note": note });
            if *no_retry {
                body["noRetry"] = json!(true);
            }
            if let Some(l) = limit_hit {
                body["limitHit"] = json!(l);
            }
            body
        }
    }
}

/// The refusal for a kind this computer doesn't run (`runJob`'s last line).
pub fn unknown_kind(kind: &str) -> JobOutcome {
    Outcome::refused(format!("This computer doesn't run {kind} jobs."))
}

/// A failure that was an error, as `runJob`'s catch words it: `The <kind> run failed: <message>`.
pub fn run_failed<R>(kind: &str, err: &dyn std::fmt::Display) -> Outcome<R> {
    Outcome::failed(format!("The {kind} run failed: {err}"))
}

/// `s` on one line (whitespace runs become one space), trimmed and cut to at most `max` characters (`oneLine`).
pub fn one_line(s: &str, max: usize) -> String {
    let cut: String = crate::text::squish(s).chars().take(max).collect();
    cut.trim().to_string()
}

/// A task's url must be the staging url this computer's own settings name: the server's word is never the only
/// check (`sameOrigin`). Only the scheme, host and port are compared.
pub fn same_origin(a: &str, b: &str) -> bool {
    match (origin_of(a), origin_of(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// `scheme://host[:port]` of an http(s) url, lower-cased, with a default port dropped (what `new URL(u).origin` says).
pub fn origin_of(url: &str) -> Option<String> {
    let u = reqwest::Url::parse(url).ok()?;
    matches!(u.scheme(), "http" | "https").then(|| u.origin().ascii_serialization())
}

/// The refusal for a task whose url isn't this project's staging url (`wrongUrl`).
pub fn wrong_url<R>(what: &str) -> Outcome<R> {
    Outcome::refused(format!("{what}: the task's url isn't this project's staging url."))
}

#[cfg(test)]
mod tests;
