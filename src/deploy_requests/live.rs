//! The real deploy request loop for one project, and the task that starts it (docs/specs/agent/deploy-requests.md
//! section 3.2). Which kinds this computer can run is [`PORTED`]: a kind joins it when its runner is ported, and the
//! loop starts only for a project once one has.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::sync::Mutex;

use super::{DeployRequests, Deps, Outcome, RunError, done, go_live, promote, testflight};
use crate::backlog::Client;
use crate::backlog::deploy::{DeployKind, DeployRequest, Finish};
use crate::config::ProjectConfig;
use crate::link::TokenStore;
use crate::view::{PauseState, StorePause, StorePauseState};

/// The kinds this computer can run: the hosting read, the go-live item, promote and the TestFlight build. A kind joins
/// it when its runner is ported, with its arm in [`Live::run`]; `can` then holds it to what it needs (a TestFlight build
/// is claimed only by a Mac with Xcode's project, the setting on and the issuer's login).
pub const PORTED: &[DeployKind] = &[
    DeployKind::Hosting,
    DeployKind::GoLivePrepare,
    DeployKind::Promote,
    DeployKind::TestflightBuild,
];

/// Kinds a paused project neither builds nor pushes (`testflight_build` and `promote`): the rest are reads and the
/// go-live item.
pub fn waits_while_paused(kind: DeployKind) -> bool {
    matches!(kind, DeployKind::TestflightBuild | DeployKind::Promote)
}

/// The real thing, for one project.
pub struct Live {
    pub project: ProjectConfig,
    /// The project's folder under the data directory, where its deploy log waits for the store (`logs.pending.json`).
    pub project_dir: PathBuf,
    /// The `gh` program (a test passes a fake).
    pub gh: PathBuf,
    pub machine: String,
    pub backlog_url: String,
    pub keychain: Arc<dyn TokenStore>,
    pub client: Client,
    pub repo_id: Mutex<Option<u64>>,
    pub paused: PauseState,
    pub store_paused: StorePauseState,
}

impl Live {
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

    /// Whether this computer holds a link to the backlog store (`o.hosted` in the TypeScript): the hosting kinds need
    /// nothing else of it.
    fn linked(&self) -> bool {
        matches!(self.keychain.get(&self.backlog_url), Ok(Some(_)))
    }

    /// The project's promote: its checkout, its branches, this `gh` and its deploy log.
    fn promoter(&self) -> crate::promote::Promoter {
        let mut p = crate::promote::Promoter::new(&self.project, &self.project_dir);
        p.gh = self.gh.clone();
        p
    }

    /// What a TestFlight build asks of this computer.
    fn testflight_tools(&self) -> crate::testflight::RealTools {
        crate::testflight::RealTools {
            repo: PathBuf::from(&self.project.repo_path),
            github_repo: self.project.github_repo.clone(),
            gh: self.gh.clone(),
        }
    }

    /// Sends the deploy log's rows to the store, unless a send was refused too lately (`due`).
    async fn send_logs(&self, due: bool) {
        let Ok((repo, token)) = self.ready().await else {
            return;
        };
        let log = self.promoter().log;
        let now = self.now_ms();
        if due {
            log.flush_due(&self.client, repo, &token, now).await;
        } else {
            log.flush(&self.client, repo, &token, now).await;
        }
    }

    pub(super) fn paused(&self) -> bool {
        let id = self.project.id_or_derived();
        self.paused.reason(&id).is_some() || matches!(self.store_paused.get(&id), StorePause::Paused(_))
    }
}

impl Deps for Live {
    fn machine(&self) -> String {
        self.machine.clone()
    }

    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }

    async fn list(&self) -> Result<Vec<DeployRequest>, String> {
        let (repo, token) = self.ready().await?;
        self.client
            .deploy_list(repo, Some(&token))
            .await
            .map_err(|e| e.to_string())
    }

    async fn can(&self, r: &DeployRequest) -> bool {
        if r.kind == DeployKind::TestflightBuild && !testflight::can(&self.project, &self.testflight_tools(), r).await {
            return false;
        }
        PORTED.contains(&r.kind)
            && !(waits_while_paused(r.kind) && self.paused())
            && !self.project.github_repo.is_empty()
            && !self.project.repo_path.is_empty()
            && self.linked()
            // Promote needs both branches and merges that go to staging first (`deployCan`).
            && (r.kind != DeployKind::Promote || crate::promote::environments_of(&self.project.deploy).is_some())
    }

    async fn claim(&self, id: &str) -> Result<bool, String> {
        let (repo, token) = self.ready().await?;
        self.client
            .deploy_claim(repo, Some(&token), id, &self.machine)
            .await
            .map_err(|e| e.to_string())
    }

    async fn beat(&self, id: &str) -> Result<bool, String> {
        let (repo, token) = self.ready().await?;
        self.client
            .deploy_beat(repo, Some(&token), id)
            .await
            .map_err(|e| e.to_string())
    }

    async fn finish(&self, id: &str, status: Finish, result: Option<Map<String, Value>>) -> Result<bool, String> {
        let (repo, token) = self.ready().await?;
        self.client
            .deploy_finish(repo, Some(&token), id, status, result)
            .await
            .map_err(|e| e.to_string())
    }

    async fn flush_logs(&self) {
        self.send_logs(true).await;
    }

    async fn run(&self, r: &DeployRequest) -> Result<Outcome, RunError> {
        match r.kind {
            DeployKind::Hosting => Ok(done(crate::hosting::read(&self.project).await)),
            DeployKind::GoLivePrepare => {
                let (repo, token) = self.ready().await.map_err(RunError::Failed)?;
                go_live::prepare(
                    &self.client,
                    repo,
                    &token,
                    &self.project.github_repo,
                    r.args.provider.as_deref(),
                )
                .await
            }
            DeployKind::Promote => {
                let out = promote::run(&self.promoter(), &self.project.deploy, r).await;
                // The row is on disk by now; send it at once rather than a pass later.
                self.send_logs(false).await;
                out
            }
            DeployKind::TestflightBuild => {
                let log = self.promoter().log;
                let out = testflight::run(&self.project, &self.testflight_tools(), &log, &self.project_dir, r).await;
                // The row is on disk by now; send it at once rather than a pass later.
                self.send_logs(false).await;
                out
            }
        }
    }
}

pub struct SpawnArgs<'a> {
    /// The data directory: each project's folder is under it.
    pub dir: &'a Path,
    pub projects: &'a [ProjectConfig],
    pub backlog_url: &'a str,
    pub keychain: &'a Arc<dyn TokenStore>,
    pub paused: &'a PauseState,
    pub store_paused: &'a StorePauseState,
    pub machine: String,
}

/// One task per project, started only when this computer can run some kind of request: a loop that can claim nothing
/// would read the store every few seconds for no one.
pub fn spawn_all(args: SpawnArgs) {
    if PORTED.is_empty() {
        return;
    }
    for project in args.projects {
        if project.github_repo.is_empty() || project.repo_path.is_empty() {
            continue;
        }
        let live = Live {
            project: project.clone(),
            project_dir: crate::paths::project_dir(args.dir, &project.id_or_derived()),
            gh: PathBuf::from("gh"),
            machine: args.machine.clone(),
            backlog_url: args.backlog_url.to_string(),
            keychain: args.keychain.clone(),
            client: Client::new(args.backlog_url),
            repo_id: Mutex::new(None),
            paused: args.paused.clone(),
            store_paused: args.store_paused.clone(),
        };
        tokio::spawn(async move {
            let mut requests = DeployRequests::new(live);
            loop {
                let wait = requests.cycle().await;
                tokio::time::sleep(wait).await;
            }
        });
    }
}
