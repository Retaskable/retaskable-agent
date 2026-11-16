//! The real deploy request loop for one project, and the task that starts it (docs/specs/agent/deploy-requests.md
//! section 3.2). Which kinds this computer can run is [`PORTED`]: a kind joins it when its runner is ported, and the
//! loop starts only for a project once one has.
use std::sync::Arc;

use serde_json::{Map, Value};
use tokio::sync::Mutex;

use super::{DeployRequests, Deps, Outcome, RunError, done, go_live};
use crate::backlog::Client;
use crate::backlog::deploy::{DeployKind, DeployRequest, Finish};
use crate::config::ProjectConfig;
use crate::link::TokenStore;
use crate::view::{PauseState, StorePause, StorePauseState};

/// The kinds this computer can run: the hosting read and the go-live item. The Rust agent has no TestFlight build or
/// promote of its own yet (docs/specs/agent/deploy-requests.md section 5), so it leaves those to a computer that has,
/// and claiming a request it can't run would be declining on the site's behalf. Add a kind here with its arm in
/// [`Live::run`].
pub const PORTED: &[DeployKind] = &[DeployKind::Hosting, DeployKind::GoLivePrepare];

/// Kinds a paused project neither builds nor pushes (`testflight_build` and `promote`): the rest are reads and the
/// go-live item.
pub fn waits_while_paused(kind: DeployKind) -> bool {
    matches!(kind, DeployKind::TestflightBuild | DeployKind::Promote)
}

/// The real thing, for one project.
pub struct Live {
    pub project: ProjectConfig,
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
            *cached = crate::repo_id::repo_id(&self.project.github_repo).await;
        }
        let repo = cached.ok_or_else(|| format!("gh couldn't say the id of {}", self.project.github_repo))?;
        Ok((repo, token))
    }

    /// Whether this computer holds a link to the backlog store (`o.hosted` in the TypeScript): the hosting kinds need
    /// nothing else of it.
    fn linked(&self) -> bool {
        matches!(self.keychain.get(&self.backlog_url), Ok(Some(_)))
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
        PORTED.contains(&r.kind)
            && !(waits_while_paused(r.kind) && self.paused())
            && !self.project.github_repo.is_empty()
            && !self.project.repo_path.is_empty()
            && self.linked()
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
            // Unreachable: `can` never says yes to a kind without an arm here.
            _ => Err(RunError::Failed(format!(
                "this computer can't run {} yet",
                r.kind.name()
            ))),
        }
    }
}

pub struct SpawnArgs<'a> {
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
