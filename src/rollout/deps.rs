//! What the rollout loop touches outside itself (docs/specs/rollout-on-server.md section 2): the store, the
//! shell, git and the staging site. A trait so the tests use fakes; [`Live`] is the real one.
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::backlog::access::{self, AccessTokens};
use crate::backlog::{Client, Item};
use crate::config::ProjectConfig;

/// How a command ended: its exit code, its output and whether it was cut at the timeout.
#[derive(Debug, Clone, Default)]
pub struct Cmd {
    pub code: i32,
    pub out: String,
    pub timed_out: bool,
}

pub trait Deps: Send + Sync {
    /// The project's deploy settings changed (the store's, under what is approved): what the report reads from now on.
    fn retarget(&mut self, _deploy: &crate::config::DeployConfig) {}
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
    /// The commit production serves, from its version path (Production only).
    fn production_served(&self) -> impl Future<Output = Result<String, String>> + Send;
    /// The promote of `sha` to production (`promote.ts`, `via: "auto"`): with `by` (a person's Deploy to production
    /// on the website) it is the website's push, whose issuer must be a repo admin. The refusal's words when it
    /// doesn't happen. Under Production only the merge is the release, so there is nothing to push.
    fn promote(&self, sha: String, by: Option<String>) -> impl Future<Output = Result<(), String>> + Send;
    /// `gh api repos/<repo>/<path>`: the JSON it prints, an error when it can't be read.
    fn gh_api(&self, path: String) -> impl Future<Output = Result<Value, String>> + Send;
}

/// The commit a deploy target's version path answers.
async fn served_by(target: &crate::config::DeployTarget, url: &str) -> Result<String, String> {
    let path = target.version_path.as_deref().unwrap_or("/version.json");
    let body = reqwest::Client::new()
        .get(format!("{url}{path}"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .text()
        .await
        .map_err(|e| e.to_string())?;
    crate::smoke::parse_deployed_sha(&body).ok_or_else(|| format!("{url}{path} named no commit"))
}

/// The real thing, for one project.
pub struct Live {
    pub project: ProjectConfig,
    pub project_dir: PathBuf,
    pub machine: String,
    pub backlog_url: String,
    pub access: Arc<dyn AccessTokens>,
    pub client: Client,
    pub repo_id: Mutex<Option<u64>>,
    /// The `gh` program (a test passes a fake).
    pub gh: PathBuf,
}

impl Live {
    /// The project's promote: its checkout, its branches, this `gh` and its deploy log (the deploy requests' own file).
    fn promoter(&self) -> crate::promote::Promoter {
        let mut p = crate::promote::Promoter::new(&self.project, &self.project_dir);
        p.gh = self.gh.clone();
        p
    }

    async fn op(&self, op: &'static str, args: Value) -> Result<Value, String> {
        let (repo, token) = self.ready().await?;
        self.client
            .op(repo, Some(&token), op, args, "answer")
            .await
            .map_err(|e| e.to_string())
    }

    async fn ready(&self) -> Result<(u64, String), String> {
        let token = match access::current(&*self.access).await {
            Ok(Some(t)) => t,
            Ok(None) => return Err("this computer holds no link to the backlog store".into()),
            Err(e) => return Err(format!("reading the backlog link: {e:#}")),
        };
        let mut cached = self.repo_id.lock().await;
        if cached.is_none() {
            *cached = crate::repo_id::repo_id(&self.project.github_repo).await;
        }
        let repo = cached.ok_or_else(|| format!("gh couldn't say the id of {}", self.project.github_repo))?;
        Ok((repo, token))
    }

    fn repo(&self) -> PathBuf {
        PathBuf::from(&self.project.repo_path)
    }
}

impl Deps for Live {
    fn retarget(&mut self, deploy: &crate::config::DeployConfig) {
        self.project.deploy = deploy.clone();
    }

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
            let (repo, token) = self.ready().await?;
            let shown = self.client.shown(repo, Some(&token)).await.map_err(|e| e.to_string())?;
            return Ok(shown
                .smoke
                .filter(|s| s.status == crate::store::SmokeStatus::Passed)
                .map(|s| s.sha));
        }
        let Some(url) = staging.url.as_deref().filter(|u| !u.is_empty()) else {
            let sha = self
                .git(vec!["rev-parse".into(), format!("origin/{}", staging.branch)])
                .await?;
            return Ok(Some(sha.trim().to_string()));
        };
        served_by(staging, url).await.map(Some)
    }

    async fn production_served(&self) -> Result<String, String> {
        let target = self.project.deploy.production.as_ref().ok_or("no production branch")?;
        let url = target.url.as_deref().filter(|u| !u.is_empty());
        served_by(target, url.ok_or("production has no address")?).await
    }

    async fn promote(&self, sha: String, by: Option<String>) -> Result<(), String> {
        if crate::promote::merges_to_production(&self.project.deploy) {
            return Ok(());
        }
        let promoter = self.promoter();
        let pushed = match by {
            Some(who) => promoter.from_site(&sha, &who).await,
            None => promoter.promote(&sha, crate::promote::Via::Auto).await,
        };
        // The row is on disk by now; the deploy request loop sends it at the latest on its next pass.
        if let Ok((repo, token)) = self.ready().await {
            let now = crate::program_status::now_ms();
            promoter.log.flush(&self.client, repo, &token, now).await;
        }
        pushed.map(|_| ()).map_err(|stop| stop.why().to_string())
    }

    async fn gh_api(&self, path: String) -> Result<Value, String> {
        let (repo, github) = (self.repo(), self.project.github_repo.clone());
        let endpoint = format!("repos/{github}/{path}");
        // A conditional GET: the report reads the same commits' checks every minute, and a 304 costs nothing.
        tokio::task::spawn_blocking(move || {
            let body = crate::github_get::get_cached_in("gh".as_ref(), &repo, &github, &endpoint)
                .map_err(|e| format!("{e:#}"))?;
            serde_json::from_str(&body).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}
