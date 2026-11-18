//! The build itself (`startTestflightBuild` and `build` in `agent/src/testflight.ts`): in a throwaway detached checkout
//! of the commit the environment serves, `npm ci` and then the fixed script `app/scripts/testflight-ios.sh` (an Xcode
//! archive and upload, never EAS), with the build number pinned first. Plain code; a command's output goes to
//! `testflight-build.log` here and never to the cloud.
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use super::setup::{app_dir, build_number_at, build_script, pin_build_number};
use super::{Environment, Running, Step, Tools, checks};
use crate::config::ProjectConfig;
use crate::promote::{DeployLog, Stop};

/// How long the script (an archive and an upload) may take.
pub const BUILD_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// How long `npm ci` may take.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const ERROR_MAX: usize = 500;
/// The store's log kind and this computer's own file for it (`testflight.log` in the project's folder).
pub const LOG_KIND: &str = "testflight";
pub const LOG_FILE: &str = "testflight.log";
/// Every step's command and output, for whoever looks at why a build failed; emptied at each build's start.
pub const BUILD_LOG_FILE: &str = "testflight-build.log";

/// How a build ended. Only `Submitted` is a success.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Result {
    Submitted,
    Failed,
    /// A restart or quit cut it.
    Cancelled,
}

impl Result {
    pub fn id(self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// `{ result, buildNumber, error? }` and the commit built, in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub sha: String,
    pub build_number: String,
    pub result: Result,
    pub error: Option<String>,
}

/// The refusals in order and the one-build-at-a-time guard, then the build, whose outcome says how it ended. A refusal
/// is the first failing one with nothing run.
pub async fn check_and_build<T: Tools>(
    project: &ProjectConfig,
    tools: &T,
    log: &DeployLog,
    project_dir: &Path,
    sha: &str,
    issued_by: &str,
    environment: Environment,
) -> std::result::Result<Outcome, Stop> {
    let full = checks::check(project, sha, issued_by, environment, tools).await?;
    let repo = PathBuf::from(&project.repo_path);
    let Some(running) = Running::take(&repo) else {
        return Err(Stop::refused("A TestFlight build is already running: wait for it."));
    };
    let ctx = Ctx {
        project,
        tools,
        project_dir,
        full: &full,
        build_number: build_number_at(tools.now_ms()),
        issued_by,
        environment,
    };
    Ok(ctx.build(log, running).await)
}

/// What one build runs against.
struct Ctx<'a, T> {
    project: &'a ProjectConfig,
    tools: &'a T,
    project_dir: &'a Path,
    /// The commit built, in full.
    full: &'a str,
    build_number: String,
    issued_by: &'a str,
    environment: Environment,
}

/// Where the throwaway checkout goes: under the project's worktrees folder, or the system's temp folder with none set.
fn checkout_dir(project: &ProjectConfig, build_number: &str) -> PathBuf {
    let root = if project.worktrees_dir.is_empty() {
        std::env::temp_dir().join("retaskable-testflight")
    } else {
        PathBuf::from(&project.worktrees_dir)
    };
    root.join(format!("testflight-{build_number}"))
}

/// The steps, in order: the checkout, the install and the script, with the build number and, for staging, its address.
fn steps(project: &ProjectConfig, full: &str, build_number: &str, environment: Environment, dir: &Path) -> Vec<Step> {
    let repo = Path::new(&project.repo_path);
    let app = app_dir(dir);
    let mut checkout = Step::new("git", &["worktree", "add", "--detach"], repo);
    checkout.args.push(dir.to_string_lossy().into_owned());
    checkout.args.push(full.to_string());
    let mut install = Step::new("npm", &["ci"], &app);
    install.timeout = Some(INSTALL_TIMEOUT);
    let script = build_script().to_string_lossy().into_owned();
    let mut archive = Step::new("bash", &[&script], &app);
    archive.timeout = Some(BUILD_TIMEOUT);
    archive
        .env
        .push(("RETASKABLE_BUILD_NUMBER".into(), build_number.into()));
    // A staging build points the app at staging's address; production's is the script's own default.
    if environment == Environment::Staging {
        let origin = project
            .deploy
            .staging
            .as_ref()
            .and_then(|s| s.url.as_deref())
            .map(|u| u.trim().trim_end_matches('/'))
            .filter(|u| !u.is_empty());
        if let Some(origin) = origin {
            archive.env.push(("RETASKABLE_CLOUD_ORIGIN".into(), origin.into()));
        }
    }
    vec![checkout, install, archive]
}

impl<T: Tools> Ctx<'_, T> {
    /// Builds the commit and writes the line to `testflight.log`, then queues it for the store. `running` is held to
    /// the end, the checkout gone by then on every path. Never fails: how it went is the outcome.
    async fn build(&self, log: &DeployLog, running: Running) -> Outcome {
        let dir = checkout_dir(self.project, &self.build_number);
        let (result, error) = match self.run_steps(&dir).await {
            Ok(()) => (Result::Submitted, None),
            Err(end) => end,
        };
        remove_checkout(self.tools, Path::new(&self.project.repo_path), &dir).await;
        drop(running);
        let outcome = Outcome {
            sha: self.full.to_string(),
            build_number: self.build_number.clone(),
            result,
            error,
        };
        // The line goes to the file first (a crash keeps it), then to the store's outbox, which prunes it from the
        // file once the store has answered it.
        let line = self.log_line(&outcome);
        let file = self.project_dir.join(LOG_FILE);
        let text = serde_json::to_string(&line).unwrap_or_default();
        if let Err(e) = append_line(&file, &text) {
            tracing::warn!("testflight: couldn't write {}: {e}", file.display());
        }
        log.add(LOG_KIND, line);
        outcome
    }

    /// Runs the steps; the end it came to when one didn't succeed.
    async fn run_steps(&self, dir: &Path) -> std::result::Result<(), (Result, Option<String>)> {
        let failed = |e: String| (Result::Failed, Some(crate::text::truncate(&e, ERROR_MAX)));
        let build_log = self.project_dir.join(BUILD_LOG_FILE);
        std::fs::create_dir_all(self.project_dir)
            .and_then(|()| std::fs::write(&build_log, ""))
            .map_err(|e| failed(e.to_string()))?;
        self.tools.fetch().await.map_err(failed)?;
        for step in steps(self.project, self.full, &self.build_number, self.environment, dir) {
            if step.program == "bash" {
                pin_build_number(&app_dir(dir), &self.build_number).map_err(|e| failed(e.to_string()))?;
            }
            let _ = append_line(&build_log, &format!("$ {}", step.line()));
            let ran = self.tools.run(&step, Some(&build_log)).await;
            if ran.ok() {
                continue;
            }
            // A restart or quit cut it: no exit code, no error text.
            if ran.cut && !ran.timed_out {
                return Err((Result::Cancelled, None));
            }
            return Err(if ran.timed_out {
                failed(format!("`{}` timed out.", step.program))
            } else {
                failed(crate::proc::tail_lines(&ran.output, 5))
            });
        }
        Ok(())
    }

    /// `{ at, sha, buildNumber, login, via: "site", environment, result, error? }`.
    fn log_line(&self, o: &Outcome) -> Map<String, Value> {
        let at = chrono::DateTime::from_timestamp_millis(self.tools.now_ms() as i64).unwrap_or_default();
        let mut line = Map::new();
        line.insert("at".into(), json!(at.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()));
        line.insert("sha".into(), json!(o.sha));
        line.insert("buildNumber".into(), json!(o.build_number));
        line.insert("login".into(), json!(self.issued_by));
        line.insert("via".into(), json!("site"));
        line.insert("environment".into(), json!(self.environment.id()));
        line.insert("result".into(), json!(o.result.id()));
        if let Some(error) = &o.error {
            line.insert("error".into(), json!(error));
        }
        line
    }
}

/// Takes the checkout away, whatever state the steps left it in: git's own removal, and then, for one that was only
/// half made (a failed `worktree add`) or that git couldn't remove, the folder and git's record of it.
async fn remove_checkout<T: Tools>(tools: &T, repo: &Path, dir: &Path) {
    let remove = Step::new("git", &["worktree", "remove", "--force", &dir.to_string_lossy()], repo);
    tools.run(&remove, None).await;
    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(dir) {
            tracing::warn!("testflight: couldn't remove {}: {e}", dir.display());
        }
        tools.run(&Step::new("git", &["worktree", "prune"], repo), None).await;
    }
}

/// Appends `text` and a newline to `file`, made if need be.
fn append_line(file: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(file)?;
    writeln!(f, "{text}")
}
