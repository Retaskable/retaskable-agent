//! The pool ops (Retaskable.com's docs/specs/pools-on-server.md section 5 and chores-on-server.md section 5.3, as built in
//! `shared/backlogPoolOps.ts`, `shared/backlogChoreTriggerOps.ts` and `shared/backlogFixes.ts`): `health`, `task_end`,
//! a claim by name (the item a task names) and `chore_trigger`, with `chore_nofix`. The agent's side is
//! `agent/src/poolReport.ts`, `poolTasks.ts`, `hostedWork.ts` (`takeTask`) and `choreLocks.ts`/`fixTasks.ts`/`smoke.ts`/
//! `orchestratorSync.ts` (the triggers). Every failure is an error, never an empty answer: a store that can't be asked
//! starts nothing.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{BacklogError, ClaimNextAgent, Client, Item};

/// What a task asks of the model, as the store names it (`Tier`, shared/pools.ts). Missing or unknown reads as `Smart`
/// (the TypeScript agent's `t.tier ?? "smart"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tier {
    #[default]
    Smart,
    Fast,
}

impl Tier {
    pub fn of(name: Option<&str>) -> Self {
        if name == Some("fast") { Self::Fast } else { Self::Smart }
    }

    /// The tier name the item flow understands (`crate::tiers`): `smart` takes the top tier the computer allows, `fast`
    /// is Sonnet at most (`capByTier` in the TypeScript agent's `tiers.ts`).
    pub fn item_tier(self) -> &'static str {
        match self {
            Self::Smart => crate::tiers::OPUS,
            Self::Fast => crate::tiers::SONNET,
        }
    }
}

/// A job a task names (`JobSpec`, shared/jobs.ts): a fixed command, run with no lane and answered with its raw result.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct JobSpec {
    pub kind: String,
    #[serde(default)]
    pub day: String,
    #[serde(default)]
    pub args: Option<Value>,
}

/// A fix task's title, and the item a playtest fix counts toward (`TaskRecord.fix`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixSpec {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub item_id: Option<String>,
    /// `playtest` or `smoke` (pools-on-server.md section 14.2).
    #[serde(default)]
    pub kind: Option<String>,
    /// A playtest fix's try-out key.
    #[serde(default)]
    pub key: Option<String>,
}

/// A `sync` task's branches: the dev branch to bring up to date, and the main branch to merge into it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SyncSpec {
    pub branch: String,
    pub main: String,
}

/// A task as `health` answers it (`TaskInput`, agent/src/poolTasks.ts). The fields the store keeps for itself (`agentId`,
/// `state`, `attempt` ...) are left alone.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskInput {
    pub id: String,
    /// The item the task is for, or (a chore, a fix, a sync) words for the log. What the task is comes from `run`; the subject
    /// is read for that only on a task with no `run`, from an older store (`pool_tasks::classify`).
    pub subject: String,
    /// What the task is (pools-on-server.md section 14.2): `item`, `chore`, `fix`, `sync` or `job`. Missing on a task an older
    /// store issued.
    #[serde(default)]
    pub run: Option<String>,
    /// A `chore` task's kind: `tidy`, `tests`, `bugs` or `fix_main`.
    #[serde(default)]
    pub kind: Option<String>,
    /// A `sync` task's dev branch and main branch.
    #[serde(default)]
    pub sync: Option<SyncSpec>,
    #[serde(default)]
    pub prompt: String,
    #[serde(default, deserialize_with = "tier_of")]
    pub tier: Tier,
    #[serde(default)]
    pub job: Option<JobSpec>,
    #[serde(default)]
    pub fix: Option<FixSpec>,
    /// A rollback's revert: runs beyond the computer's session limit.
    #[serde(default)]
    pub extra_slot: bool,
    /// The store's order of the tasks it sends (section 18.6): a main sync and a rollback's revert first, then the order it
    /// issued them. Tasks start in it, and the agent keeps no ranking of its own within a project. Missing on an older store's task,
    /// which starts after those that have one, in the order sent.
    #[serde(default = "no_order")]
    pub order: u32,
}

fn no_order() -> u32 {
    u32::MAX
}

fn tier_of<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Tier, D::Error> {
    Ok(Tier::of(Option::<String>::deserialize(d)?.as_deref()))
}

/// What `health` answers: the tasks to run (new ones, and those sent again because this computer hasn't reported them
/// running), those to stop, and how many more it may take.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthAnswer {
    #[serde(default)]
    pub tasks: Vec<TaskInput>,
    /// Tasks whose lease the store let go: a session still running for one is to stop.
    #[serde(default)]
    pub stop: Vec<String>,
    #[serde(default)]
    pub free: u32,
    #[serde(default)]
    pub allowed: u32,
    /// Why nothing is sent now (the 5-hour window, a pause); its first words are the kind, a figure follows in brackets.
    #[serde(default)]
    pub wait: Option<String>,
    /// Every number the agent used to hold (`AGENT_NUMBERS`, pools-on-server.md section 14.6): `None` when the answer carries
    /// none or not all of them (an older store), and then nothing that needs one starts.
    #[serde(default, deserialize_with = "crate::store_tuning::StoreTuning::lenient")]
    pub tuning: Option<crate::store_tuning::StoreTuning>,
}

/// One task this computer runs, as `health`'s `running` lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningTask {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    /// `needs_human` for a chore that stopped and waits for the store's call (pools-on-server.md section 14.5), with `why`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
}

impl RunningTask {
    /// A task that runs.
    pub fn running(id: String) -> Self {
        Self {
            id,
            cost_usd: None,
            state: None,
            why: None,
        }
    }

    /// A chore that stopped, parked until the store answers `stop` for it (or lets it be).
    pub fn stopped(id: String, why: String) -> Self {
        Self {
            state: Some("needs_human"),
            why: Some(why),
            ..Self::running(id)
        }
    }
}

/// Claude's 5-hour and weekly use as `health` sends it (`PoolUsage`, shared/pools.ts): shares from 0 to 1, resets in epoch
/// seconds. Only a reading that is current is sent.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolUsage {
    pub status: String,
    pub utilization: Option<f64>,
    pub resets_at: Option<i64>,
    pub week_utilization: Option<f64>,
    pub week_resets_at: Option<i64>,
}

/// A problem of this computer's own before any session ran (a worktree that would not start), reported in every project's
/// `health` until a session starts well (pools-on-server.md section 14.4). The store holds the pause; the agent has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OwnFailure {
    /// When it happened, in epoch milliseconds.
    pub at: i64,
    pub why: String,
}

/// The body of `health` (`PoolReporter.report` in agent/src/poolReport.ts).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    /// This agent takes the tasks the store sends: the store sends it new work only when it says so.
    pub obeys: bool,
    /// What this computer can run for the project's jobs (`repo`, `engine`, `chrome`, `scanToken`): facts only.
    pub can: Vec<String>,
    pub project: String,
    pub agent: ClaimNextAgent,
    pub running: Vec<RunningTask>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<PoolUsage>,
    /// Each engine's limit hit still holding: engine to epoch milliseconds.
    pub limits: BTreeMap<String, i64>,
    /// Today's spend, by project.
    pub spend: BTreeMap<String, f64>,
    /// Why this computer takes no new work for its own reasons (`budget`, `signed out`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    pub paused: bool,
    pub draining: bool,
    /// Left out once a session has started well.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub own_failure: Option<OwnFailure>,
    /// What this computer knows of the base branch (pools-on-server.md section 15.1): facts only; none before the first fetch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub main: Option<crate::main_look::MainFacts>,
}

/// How a task ended (`task_end`'s fields beside the id): `ok`, and for a refusal or a failure its words.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskEnd {
    pub id: String,
    pub ok: bool,
    /// A task this computer can't run, in words (a job kind it doesn't have).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<Refused>,
    /// A failure's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// What a job adds beside `ok` (`result`, `known`, `noRetry`, `limitHit`): its end body as `jobs::end_body` made it.
    #[serde(default, flatten, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Refused {
    pub why: String,
}

impl TaskEnd {
    pub fn done(id: &str, ok: bool) -> Self {
        Self {
            id: id.to_string(),
            ok,
            refused: None,
            note: None,
            extra: serde_json::Map::new(),
        }
    }

    /// A job's end, from the body `jobs::end_body` made (`ok`, then `result`, `known`, `refused`, `note`, `noRetry`,
    /// `limitHit`): the fields `task_end` carries beside the id.
    pub fn from_job_body(id: &str, body: &Value) -> Self {
        let mut end = Self::done(id, body.get("ok").and_then(Value::as_bool).unwrap_or(false));
        end.refused = body
            .get("refused")
            .and_then(|r| r.get("why"))
            .and_then(Value::as_str)
            .map(|why| Refused { why: why.to_string() });
        end.note = body.get("note").and_then(Value::as_str).map(str::to_string);
        for key in ["result", "known", "noRetry", "limitHit"] {
            if let Some(v) = body.get(key) {
                end.extra.insert(key.to_string(), v.clone());
            }
        }
        end
    }

    pub fn refused(id: &str, why: impl Into<String>) -> Self {
        Self {
            refused: Some(Refused { why: why.into() }),
            ..Self::done(id, false)
        }
    }
}

/// What `task_end` answers: `held: false` when the store no longer holds the task (its lease ran out first), which a
/// caller drops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct TaskEndAnswer {
    pub held: bool,
}

/// What a claim by name answers: the item with the lease this computer now holds. The store already claimed it for this
/// computer when it made the task, so it answers its own token.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Claimed {
    pub item: Item,
    pub token: String,
}

/// A fact only a computer can see, told to the store, which decides whether a chore follows (`chore_trigger`).
#[derive(Debug, Clone, PartialEq)]
pub enum Trigger {
    /// Staging's smoke failed at `sha`; any computer may run the fix.
    StagingSmoke {
        sha: String,
        url: String,
        command: String,
        tail: String,
    },
    /// Main has commits the base branch lacks. Its arguments are flat, not under `data`.
    BehindMain {
        branch: String,
        main: String,
        behind: u32,
        project: String,
    },
}

/// `text` cut to the store's limit for its field (`str`/`optStr` in shared/backlogRun.ts count UTF-16 units): a field too
/// long is a 400 that would drop the whole report. `keep_end` keeps the last characters (a log's tail), else the first.
fn cut(text: &str, max: usize, keep_end: bool) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= max {
        return text.to_string();
    }
    let part = if keep_end {
        &units[units.len() - max..]
    } else {
        &units[..max]
    };
    String::from_utf16_lossy(part).trim_matches('\u{FFFD}').to_string()
}

impl Trigger {
    /// The `chore_trigger` arguments (`TRIGGERS` in shared/backlogChoreTriggerOps.ts), each field within the limit the
    /// store checks. (The TypeScript agent sends them as they are.)
    pub fn args(&self) -> Value {
        let first = |s: &str, max| cut(s, max, false);
        match self {
            Self::StagingSmoke {
                sha,
                url,
                command,
                tail,
            } => {
                json!({ "trigger": "staging_smoke", "data": { "sha": first(sha, 64), "url": first(url, 500), "command": first(command, 1000), "tail": cut(tail, 20_000, true) } })
            }
            Self::BehindMain {
                branch,
                main,
                behind,
                project,
            } => {
                json!({ "trigger": "behind_main", "branch": first(branch, 200), "main": first(main, 200), "behind": behind, "project": first(project, 100) })
            }
        }
    }
}

/// The commit a fix main found nothing to fix at (`chores_get`'s `noFixSha`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NoFix {
    pub sha: String,
    pub branch: String,
}

/// What `chore_trigger` answers: why no chore follows, or what was queued.
#[derive(Debug, Clone, PartialEq)]
pub enum TriggerAnswer {
    Skipped(String),
    /// Queued for a computer: `health` sends it as a task.
    Queued,
    /// A task made at once (`behind_main`'s sync).
    Task(String),
}

impl TriggerAnswer {
    fn of(value: &Value) -> Option<Self> {
        let o = value.as_object()?;
        if let Some(why) = o.get("skipped").and_then(Value::as_str) {
            return Some(Self::Skipped(why.to_string()));
        }
        if let Some(task) = o.get("task").and_then(Value::as_str) {
            return Some(Self::Task(task.to_string()));
        }
        o.contains_key("queued").then_some(Self::Queued)
    }
}

impl Client {
    /// `health` (pools-on-server.md section 5.1): the computer's report, which renews the lease of each task it lists as
    /// running and answers the tasks to run.
    pub async fn health(
        &self,
        repo_id: u64,
        token: Option<&str>,
        report: &Health,
    ) -> Result<HealthAnswer, BacklogError> {
        let args = serde_json::to_value(report).map_err(|e| BacklogError::BadAnswer {
            status: 0,
            code: None,
            message: format!("couldn't write the health report: {e}"),
        })?;
        self.op(repo_id, token, "health", args, "health answer").await
    }

    /// `task_end { id, ok, ... }` (section 5.2). `held: false` is the store dropping an end after the lease ran out: the
    /// caller drops it too. Any error is the end not heard: the caller keeps it and sends it again.
    pub async fn task_end(
        &self,
        repo_id: u64,
        token: Option<&str>,
        end: &TaskEnd,
    ) -> Result<TaskEndAnswer, BacklogError> {
        let args = serde_json::to_value(end).unwrap_or_default();
        self.op(repo_id, token, "task_end", args, "task end answer").await
    }

    /// `claim { id, agent }`: the item a task names, claimed by name (`HostedWork.takeTask`). Refusals the caller ends the
    /// task for: `conflict` (another computer holds it), `not_found`, `held_by_owner` (`REFUSED` in poolTasks.ts).
    pub async fn claim_task(
        &self,
        repo_id: u64,
        token: Option<&str>,
        id: &str,
        agent: &ClaimNextAgent,
    ) -> Result<Claimed, BacklogError> {
        self.op(
            repo_id,
            token,
            "claim",
            json!({ "id": id, "agent": agent }),
            "claimed item",
        )
        .await
    }

    /// `chore_trigger` (chores-on-server.md section 5.3): tells the store what only this computer can see; it decides
    /// whether a chore follows, and sends it back through `health`.
    pub async fn chore_trigger(
        &self,
        repo_id: u64,
        token: Option<&str>,
        trigger: &Trigger,
    ) -> Result<TriggerAnswer, BacklogError> {
        let (status, body) = self.send(repo_id, token, "chore_trigger", trigger.args()).await?;
        body.value
            .as_ref()
            .and_then(TriggerAnswer::of)
            .ok_or_else(|| BacklogError::BadAnswer {
                status,
                code: None,
                message: "couldn't read what the store did with the trigger".into(),
            })
    }

    /// `chores_get`: the commit a fix main found nothing to fix at (`noFixSha`), which every computer stops reporting
    /// (taken from the earlier attempt in the parity-pools worktree).
    pub async fn no_fix_sha(&self, repo_id: u64, token: Option<&str>) -> Result<Option<NoFix>, BacklogError> {
        let day: Value = self
            .op(repo_id, token, "chores_get", json!({}), "chores_get answer")
            .await?;
        Ok(day.get("noFixSha").and_then(|n| serde_json::from_value(n.clone()).ok()))
    }

    /// `chore_nofix { sha, branch }`: a fix main found nothing to fix at `sha`, so no computer queues one for it again.
    pub async fn chore_nofix(
        &self,
        repo_id: u64,
        token: Option<&str>,
        sha: &str,
        branch: &str,
    ) -> Result<(), BacklogError> {
        self.op_done(repo_id, token, "chore_nofix", json!({ "sha": sha, "branch": branch }))
            .await
    }
}

#[cfg(test)]
pub(crate) mod contract;
#[cfg(test)]
mod tests;
