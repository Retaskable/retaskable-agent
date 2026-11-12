//! Rollout on the server (docs/specs/rollout-on-server.md): the store's machine decides every step of a release;
//! this computer reports what only git can say, polls, and runs exactly the one instruction it is given
//! (`promote`, `traffic <percent>` or `health`) with no model session. Nothing is kept across a restart, and a
//! store that can't be reached means nothing runs (fail closed).
pub mod deps;
mod live;
pub mod report;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::Instant;

use crate::config::ProjectConfig;
pub use deps::{Cmd, Deps, Live};
pub use live::{SpawnArgs, spawn_all};

/// How long to wait when the store says nothing about it.
pub(crate) const WAIT: Duration = Duration::from_secs(5);
/// How long an instruction's command may run when it names no (good) `timeoutMs`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// `health` is never run more often than this, whatever the store asks (it guards Cloudflare's API).
pub const HEALTH_MIN: Duration = Duration::from_secs(10);
/// A command running longer than this has its runner beat.
const BEAT_AFTER: Duration = Duration::from_secs(10);
const BEAT_EVERY: Duration = Duration::from_secs(15);
/// How often the report is made again (it is sent only when it changed).
const REPORT_EVERY: Duration = Duration::from_secs(60);
const HEALTH_LINE_MAX: usize = 2048;
const WHY_MAX: usize = 300;

/// The project's staging and production branches when it rolls out through the store: both rollout commands are
/// set, and there is a branch to release from. A saved `site` flag is not read.
pub fn uses_store(cfg: &ProjectConfig) -> Result<(String, Option<String>), &'static str> {
    let rollout = cfg.deploy.production.as_ref().and_then(|p| p.rollout.as_ref());
    let set = |c: &Option<String>| c.as_deref().is_some_and(|c| !c.trim().is_empty());
    if !rollout.is_some_and(|r| set(&r.traffic) && set(&r.health)) {
        return Err("no rollout: its settings don't have both a traffic and a health command");
    }
    let branch =
        |t: &Option<crate::config::DeployTarget>| t.as_ref().map(|t| t.branch.clone()).filter(|b| !b.is_empty());
    let staging = branch(&cfg.deploy.staging);
    match branch(&cfg.deploy.production) {
        Some(production) => Ok((production, staging)),
        None => Err("no rollout: the project has no production branch"),
    }
}

/// What the store's poll answers.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Answer {
    held: Option<bool>,
    epoch: Option<Value>,
    #[serde(rename = "do")]
    instruction: Option<Instruction>,
    idle: Option<Value>,
    why: Option<String>,
    after_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Instruction {
    id: Value,
    command: String,
    #[serde(default)]
    sha: Value,
    percent: Option<Value>,
    timeout_ms: Option<Value>,
}

impl Instruction {
    fn timeout(&self) -> Duration {
        match self.timeout_ms.as_ref().and_then(Value::as_u64) {
            Some(ms) if ms > 0 => Duration::from_millis(ms),
            _ => DEFAULT_TIMEOUT,
        }
    }
}

fn is_sha(v: &Value) -> bool {
    v.as_str()
        .is_some_and(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// The last non-empty line of `text`, cut to its last `max` characters.
fn last_line(text: &str, max: usize) -> Option<String> {
    let line = text.lines().rev().map(str::trim).find(|l| !l.is_empty())?;
    let skip = line.chars().count().saturating_sub(max);
    Some(line.chars().skip(skip).collect())
}

fn result(id: &Value, ok: bool, why: Option<String>, line: Option<String>) -> Value {
    let mut r = json!({ "id": id, "ok": ok });
    if let Some(why) = why {
        r["why"] = json!(why);
    }
    if let Some(line) = line {
        r["line"] = json!(line);
    }
    r
}

fn failed(id: &Value, why: &str) -> Value {
    result(id, false, Some(why.to_string()), None)
}

/// One project's rollout loop.
pub struct Rollout<D: Deps> {
    deps: D,
    traffic: Option<String>,
    health: Option<String>,
    production: String,
    staging: Option<String>,
    /// The last result, sent on every poll until the store moves on: `(instruction id, result)`.
    kept: Option<(Value, Value)>,
    epoch: Option<Value>,
    last_health: Option<Instant>,
    last_report: Option<Instant>,
    sent_report: Option<Value>,
    logged: HashMap<&'static str, String>,
    sink: Box<dyn Fn(&str) + Send + Sync>,
}

impl<D: Deps> Rollout<D> {
    pub fn new(deps: D, cfg: &ProjectConfig, sink: Box<dyn Fn(&str) + Send + Sync>) -> Result<Self, &'static str> {
        let (production, staging) = uses_store(cfg)?;
        let rollout = cfg
            .deploy
            .production
            .as_ref()
            .and_then(|p| p.rollout.clone())
            .unwrap_or_default();
        Ok(Self {
            deps,
            traffic: rollout.traffic,
            health: rollout.health,
            production,
            staging,
            kept: None,
            epoch: None,
            last_health: None,
            last_report: None,
            sent_report: None,
            logged: HashMap::new(),
            sink,
        })
    }

    /// Takes the commands as they are allowed now.
    pub fn apply(&mut self, cfg: &ProjectConfig) {
        let rollout = cfg
            .deploy
            .production
            .as_ref()
            .and_then(|p| p.rollout.clone())
            .unwrap_or_default();
        self.traffic = rollout.traffic;
        self.health = rollout.health;
    }

    /// Says `line` unless `key`'s last line was the same.
    fn log(&mut self, key: &'static str, line: String) {
        if self.logged.get(key) != Some(&line) {
            (self.sink)(&line);
            self.logged.insert(key, line);
        }
    }

    /// One round: the report when it is due, then one poll and what it asks. Returns how long to wait.
    pub async fn cycle(&mut self) -> Duration {
        if self.last_report.is_none_or(|at| at.elapsed() >= REPORT_EVERY) {
            self.last_report = Some(Instant::now());
            self.report().await;
        }
        self.step().await
    }

    async fn report(&mut self) {
        let built = match self.deps.fetch().await {
            Ok(()) => report::build(&self.deps, &self.production, self.staging.as_deref()).await,
            Err(e) => Err(e),
        };
        let report = match built {
            Ok(r) => r,
            Err(e) => return self.log("report", format!("nothing reported: {e}")),
        };
        if self.sent_report.as_ref() == Some(&report) {
            return;
        }
        match self.deps.report(report.clone()).await {
            Ok(()) => {
                self.log("report", report::summary(&report));
                self.sent_report = Some(report);
            }
            Err(e) => self.log("report", format!("nothing reported: {e}")),
        }
    }

    /// One poll, and the instruction it brings, if any.
    pub async fn step(&mut self) -> Duration {
        let kept = self.kept.as_ref().map(|(_, r)| r.clone());
        let answer = match self.deps.poll(kept).await.map(serde_json::from_value::<Answer>) {
            Ok(Ok(a)) => a,
            Ok(Err(e)) => return self.poll_error(format!("couldn't read the answer: {e}")),
            Err(e) => return self.poll_error(e),
        };
        if answer.epoch.is_some() {
            self.epoch = answer.epoch.clone();
        }
        let after = |a: &Answer| a.after_ms.filter(|&ms| ms > 0).map_or(WAIT, Duration::from_millis);
        if answer.held == Some(false) {
            self.kept = None;
            self.log("poll", "another computer runs the rollout".into());
            return WAIT;
        }
        let Some(instruction) = answer.instruction.clone() else {
            let why = answer
                .why
                .clone()
                .or_else(|| answer.idle.as_ref().and_then(|v| v.as_str().map(String::from)));
            self.log("poll", format!("idle: {}", why.unwrap_or_default()));
            return after(&answer);
        };
        if self.kept.as_ref().is_some_and(|(id, _)| *id == instruction.id) {
            return after(&answer);
        }
        let sha = instruction.sha.as_str().unwrap_or_default();
        let sha9 = &sha[..sha.len().min(9)];
        let told = match instruction
            .percent
            .as_ref()
            .filter(|_| instruction.command == "traffic")
        {
            Some(p) => format!("told to traffic {p} {sha9}"),
            None => format!("told to {} {sha9}", instruction.command),
        };
        self.log("poll", told);
        self.act(&instruction).await
    }

    fn poll_error(&mut self, e: String) -> Duration {
        self.log("poll", format!("poll error: {e}"));
        WAIT
    }

    /// Runs one instruction and keeps its result; says how long to wait before the next poll.
    async fn act(&mut self, i: &Instruction) -> Duration {
        let name = i.command.as_str();
        let command = match name {
            _ if !is_sha(&i.sha) => return self.finish(i, failed(&i.id, "that isn't a commit")),
            "traffic" => {
                let percent = i.percent.as_ref().and_then(Value::as_u64).filter(|p| *p <= 100);
                match (self.traffic.as_deref(), percent) {
                    (Some(t), Some(p)) => format!("{t} {p}"),
                    _ => return self.finish(i, failed(&i.id, "no traffic command, or a bad percent")),
                }
            }
            "health" => {
                if let Some(wait) = self
                    .last_health
                    .and_then(|at| HEALTH_MIN.checked_sub(at.elapsed()).filter(|w| !w.is_zero()))
                {
                    self.log(
                        "health",
                        format!("health asked again too soon, waits {}s", wait.as_secs() + 1),
                    );
                    return wait;
                }
                match self.health.clone() {
                    Some(h) => h,
                    None => return self.finish(i, failed(&i.id, "no health command")),
                }
            }
            // Production only: the merge is the release and its build is uploaded already.
            "promote" if self.staging.is_none() => return self.finish(i, result(&i.id, true, None, None)),
            "promote" => return self.finish(i, failed(&i.id, "this computer can't promote yet")),
            other => return self.finish(i, failed(&i.id, &format!("unknown instruction {other}"))),
        };
        if name == "health" {
            self.last_health = Some(Instant::now());
        }
        let Some(out) = self.run_beating(command, i.timeout()).await else {
            return WAIT;
        };
        let r = if out.timed_out {
            failed(&i.id, "timed out")
        } else if out.code == 0 {
            let line = (name == "health")
                .then(|| last_line(&out.out, HEALTH_LINE_MAX))
                .flatten();
            result(&i.id, true, None, line)
        } else {
            let why = last_line(&out.out, WHY_MAX).unwrap_or_else(|| format!("exit {}", out.code));
            failed(&i.id, &why)
        };
        self.finish(i, r)
    }

    /// Keeps and logs a result; the next poll goes out at once.
    fn finish(&mut self, i: &Instruction, r: Value) -> Duration {
        let sha = i.sha.as_str().unwrap_or_default();
        let what = format!("{} {}", i.command, &sha[..sha.len().min(9)]);
        let line = match (r["ok"] == true, r["why"].as_str()) {
            (true, _) => format!("{what}: ok"),
            (false, why) => format!("{what}: failed, {}", why.unwrap_or_default()),
        };
        self.log("result", line);
        self.kept = Some((i.id.clone(), r));
        Duration::ZERO
    }

    /// Runs the command; while it takes longer than 10 seconds beats every 15. `None` when a beat said this
    /// computer no longer runs the rollout: the result is dropped.
    async fn run_beating(&mut self, command: String, timeout: Duration) -> Option<Cmd> {
        let epoch = self.epoch.clone().unwrap_or(Value::Null);
        let out = {
            let run = self.deps.run(command, timeout);
            tokio::pin!(run);
            let mut next = BEAT_AFTER;
            loop {
                tokio::select! {
                    out = &mut run => break Some(out),
                    _ = tokio::time::sleep(next) => {
                        next = BEAT_EVERY;
                        if let Ok(a) = self.deps.beat(epoch.clone()).await && a["held"] == false {
                            break None;
                        }
                    }
                }
            }
        };
        if out.is_none() {
            self.kept = None;
            self.log("result", "no longer the runner, result dropped".into());
        }
        out
    }
}
