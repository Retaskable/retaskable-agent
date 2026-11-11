//! What the rollout loop touches outside itself (docs/specs/rollout-on-server.md section 2): the store, the
//! shell, git and the staging site. A trait so the tests use fakes; [`Live`] is the real one.
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::backlog::{Client, Item};
use crate::config::ProjectConfig;
use crate::link::TokenStore;

/// How a command ended: its exit code, its output and whether it was cut at the timeout.
#[derive(Debug, Clone, Default)]
pub struct Cmd {
    pub code: i32,
    pub out: String,
    pub timed_out: bool,
}

pub trait Deps: Send + Sync {
    /// `rollout_poll { machine, result? }`: the store's answer.
    fn poll(&self, result: Option<Value>) -> impl Future<Output = Result<Value, String>> + Send;
    /// `rollout_beat { epoch }`: the store's answer (`{ held }`).
    fn beat(&self, epoch: Value) -> impl Future<Output = Result<Value, String>> + Send;
    /// `rollout_ready { production, commits }`.
    fn report(&self, report: Value) -> impl Future<Output = Result<(), String>> + Send;
    /// Runs `command` through the shell in the project's checkout.
    fn run(&self, command: String, timeout: Duration) -> impl Future<Output = Cmd> + Send;
    /// Fetches the project's branches.
    fn fetch(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// git's stdout, an error when it exits non-zero.
    fn git(&self, args: Vec<String>) -> impl Future<Output = Result<String, String>> + Send;
    /// `git merge-base --is-ancestor a b`.
    fn is_ancestor(&self, a: String, b: String) -> impl Future<Output = Result<bool, String>> + Send;
    /// The project's items, from the store.
    fn items(&self) -> impl Future<Output = Result<Vec<Item>, String>> + Send;
    /// The commit staging serves: with a smoke the last one that passed (`None`: none has), else what its version
    /// path answers, else `origin/<staging branch>`.
    fn staging_served(&self) -> impl Future<Output = Result<Option<String>, String>> + Send;
}

/// The real thing, for one project.
pub struct Live {
    pub project: ProjectConfig,
    pub project_dir: PathBuf,
    pub machine: String,
    pub backlog_url: String,
    pub keychain: Arc<dyn TokenStore>,
    pub client: Client,
    pub repo_id: Mutex<Option<u64>>,
}

impl Live {
    async fn op(&self, op: &'static str, args: Value) -> Result<Value, String> {
        let (repo, token) = self.ready().await?;
        self.client
            .op(repo, Some(&token), op, args, "answer")
            .await
            .map_err(|e| e.to_string())
    }

    async fn ready(&self) -> Result<(u64, String), String> {
        let token = match self.keychain.get(&self.backlog_url) {
            Ok(Some(t)) => t,
            Ok(None) => return Err("this computer holds no link to the backlog store".into()),
            Err(e) => return Err(format!("reading the backlog link: {e:#}")),
        };
        let mut cached = self.repo_id.lock().await;
        if cached.is_none() {
            *cached = crate::orchestrator::driver::repo_id(&self.project.github_repo).await;
        }
        let repo = cached.ok_or_else(|| format!("gh couldn't say the id of {}", self.project.github_repo))?;
        Ok((repo, token))
    }

    fn repo(&self) -> PathBuf {
        PathBuf::from(&self.project.repo_path)
    }
}

impl Deps for Live {
    async fn poll(&self, result: Option<Value>) -> Result<Value, String> {
        let mut args = json!({ "machine": self.machine });
        if let Some(r) = result {
            args["result"] = r;
        }
        self.op("rollout_poll", args).await
    }

    async fn beat(&self, epoch: Value) -> Result<Value, String> {
        self.op("rollout_beat", json!({ "epoch": epoch })).await
    }

    async fn report(&self, report: Value) -> Result<(), String> {
        self.op("rollout_ready", report).await.map(|_| ())
    }

    async fn run(&self, command: String, timeout: Duration) -> Cmd {
        let cwd = self.repo();
        let done = tokio::task::spawn_blocking(move || crate::proc::run_command_with_timeout(&command, &cwd, timeout));
        match done.await {
            Ok(Ok((code, out, timed_out))) => Cmd { code, out, timed_out },
            Ok(Err(e)) => Cmd {
                code: -1,
                out: format!("{e:#}"),
                timed_out: false,
            },
            Err(e) => Cmd {
                code: -1,
                out: e.to_string(),
                timed_out: false,
            },
        }
    }

    async fn fetch(&self) -> Result<(), String> {
        let repo = self.repo();
        tokio::task::spawn_blocking(move || crate::lanes::fetch_latest(&repo).map(|_| ()))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("{e:#}"))
    }

    async fn git(&self, args: Vec<String>) -> Result<String, String> {
        let repo = self.repo();
        tokio::task::spawn_blocking(move || {
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            crate::lanes::git_output_in_worktree(&repo, &args)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
    }

    async fn is_ancestor(&self, a: String, b: String) -> Result<bool, String> {
        let repo = self.repo();
        tokio::task::spawn_blocking(move || {
            let out = crate::git_bin::command(&repo)
                .args(["merge-base", "--is-ancestor", &a, &b])
                .output()
                .map_err(|e| e.to_string())?;
            match out.status.code() {
                Some(0) => Ok(true),
                Some(1) => Ok(false),
                _ => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
            }
        })
        .await
        .map_err(|e| e.to_string())?
    }

    async fn items(&self) -> Result<Vec<Item>, String> {
        let (repo, token) = self.ready().await?;
        self.client.list(repo, Some(&token)).await.map_err(|e| e.to_string())
    }

    async fn staging_served(&self) -> Result<Option<String>, String> {
        let staging = self.project.deploy.staging.as_ref().ok_or("no staging branch")?;
        if staging.smoke.is_some() {
            let store = crate::store::Store::load(&self.project_dir).map_err(|e| format!("{e:#}"))?;
            return Ok(store
                .smoke()
                .filter(|s| s.status == crate::store::SmokeStatus::Passed)
                .map(|s| s.sha));
        }
        let Some(url) = staging.url.as_deref().filter(|u| !u.is_empty()) else {
            let sha = self
                .git(vec!["rev-parse".into(), format!("origin/{}", staging.branch)])
                .await?;
            return Ok(Some(sha.trim().to_string()));
        };
        let path = staging.version_path.as_deref().unwrap_or("/version.json");
        let body = reqwest::Client::new()
            .get(format!("{url}{path}"))
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())?;
        crate::smoke::parse_deployed_sha(&body)
            .map(Some)
            .ok_or_else(|| format!("{url}{path} named no commit"))
    }
}
