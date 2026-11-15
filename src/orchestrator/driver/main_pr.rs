//! Open PR to main (docs/specs/intent-rows.md section 4, `main_pr_requested`; `openMainPr` in the TS agent's
//! `teamBranchActions.ts`): a person presses it on a project working on a team branch, and the pull request from the
//! base branch into the default branch opens. The intent puller claims the row and the handler asks this loop
//! (`intents/main_pr.rs`); the loop answers on its next pass, because it alone knows the base branch and the store's
//! items. No session and no slot, and a paused project still opens it (the person asked). Merge pattern only.
//!
//! Where the TS agent reads the team branch after each fetch and keeps it, this reads it afresh for the request
//! (fetch, then `team_branch::read`), and then asks `gh` for the open pull request once more before creating one, so
//! the refusals come in the TS agent's order and nothing opens twice from one stale look.
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio::sync::mpsc;

use super::Shared;
use super::project::ProjectLoop;
use crate::backlog::Item;
use crate::config::ProjectConfig;
use crate::env::Env;
use crate::intents::main_pr::{Answer, MainPrHandler, OpenMainPr, Why};
use crate::intents::{IntentStore, Puller, StoreLink};
use crate::team_branch::main_pr::{Draft, create, draft_main_pr, found_open, open_refusal};
use crate::team_branch::{MainPr, TeamBranch, need_team_branch, open_main_pr, read};

/// How often the store is asked for main PR rows (docs/specs/intent-rows.md section 3.3).
const POLL_EVERY: Duration = Duration::from_secs(60);

/// What opening needs from git and the code host. Faked in tests.
pub trait MainPrHost: Send + Sync {
    /// Fetches, then reads the team branch of `project` against `main`; `items` are the store's.
    fn read(&self, project: &ProjectConfig, main: &str, items: &[Item]) -> Result<TeamBranch>;
    /// The open pull request from the base branch into `main`, asked of the host now.
    fn open_pr(&self, project: &ProjectConfig, main: &str) -> Result<Option<MainPr>>;
    /// What the pull request would say.
    fn draft(&self, project: &ProjectConfig, main: &str) -> Result<Draft>;
    /// Opens it; its URL.
    fn create(&self, project: &ProjectConfig, main: &str, draft: &Draft) -> Result<String>;
}

/// The real thing: git, and `gh` (or `RETASKABLE_GH`), run in the project's checkout.
pub struct GhMainPrHost {
    pub env: Env,
}

impl GhMainPrHost {
    fn gh(&self) -> std::path::PathBuf {
        self.env
            .get("RETASKABLE_GH")
            .map_or_else(|| "gh".into(), std::path::PathBuf::from)
    }
}

impl MainPrHost for GhMainPrHost {
    fn read(&self, p: &ProjectConfig, main: &str, items: &[Item]) -> Result<TeamBranch> {
        let dir = std::path::Path::new(&p.repo_path);
        crate::lanes::fetch_latest(dir)?;
        read(&self.gh(), dir, &p.github_repo, &p.base_branch, main, items)
    }

    fn open_pr(&self, p: &ProjectConfig, main: &str) -> Result<Option<MainPr>> {
        let dir = std::path::Path::new(&p.repo_path);
        open_main_pr(&self.gh(), dir, &p.github_repo, &p.base_branch, main)
    }

    fn draft(&self, p: &ProjectConfig, main: &str) -> Result<Draft> {
        let dir = std::path::Path::new(&p.repo_path);
        draft_main_pr(&self.gh(), dir, &p.github_repo, &p.base_branch, main)
    }

    fn create(&self, p: &ProjectConfig, main: &str, draft: &Draft) -> Result<String> {
        let dir = std::path::Path::new(&p.repo_path);
        create(&self.gh(), dir, &p.github_repo, &p.base_branch, main, draft)
    }
}

/// A project's Open PR to main requests: the channel its handler writes to, the puller (built the first time a pass
/// runs) and when the store was last asked.
pub(super) struct Opening {
    asks: mpsc::UnboundedSender<OpenMainPr>,
    waiting: mpsc::UnboundedReceiver<OpenMainPr>,
    puller: Option<Puller<()>>,
    polled: Option<Instant>,
    /// How long after one look at the store the next may be.
    pub(super) poll_every: Duration,
}

impl Default for Opening {
    fn default() -> Self {
        let (asks, waiting) = mpsc::unbounded_channel();
        Self {
            asks,
            waiting,
            puller: None,
            polled: None,
            poll_every: POLL_EVERY,
        }
    }
}

fn refused(why: impl Into<String>) -> Why {
    Why::Refused(why.into())
}

impl ProjectLoop {
    /// Answers the Open PR to main requests the puller handed over, then looks for new rows.
    pub(super) async fn main_pr_pass(&mut self, host: &Arc<dyn MainPrHost>, shared: &Shared<'_>) {
        while let Ok(ask) = self.opening.waiting.try_recv() {
            let answer = self.open_main_pr(host, shared).await;
            let _ = ask.reply.send(answer);
        }
        if shared.stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        if self
            .opening
            .polled
            .is_some_and(|at| at.elapsed() < self.opening.poll_every)
        {
            return;
        }
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        self.opening.polled = Some(Instant::now());
        let link: Arc<dyn IntentStore> = Arc::new(StoreLink {
            client: self.client.clone(),
            repo,
            token,
            machine: shared.links.machine.clone(),
            failures: self.failures.clone(),
        });
        let handler = Arc::new(MainPrHandler::new(self.opening.asks.clone()));
        let puller = self
            .opening
            .puller
            .get_or_insert_with(|| Puller::default().register(handler));
        puller.pass(&link, || ()).await;
    }

    /// Opens the pull request from the base branch into main, or says why it can't, in the TS agent's order.
    async fn open_main_pr(&mut self, host: &Arc<dyn MainPrHost>, shared: &Shared<'_>) -> Answer {
        if self.seen.default_branch.is_none() {
            self.seen.default_branch =
                super::report::default_branch(&self.project.github_repo, std::path::Path::new(&self.project.repo_path))
                    .await;
        }
        let main = need_team_branch(&self.project.base_branch, self.seen.default_branch.as_deref())
            .map_err(refused)?
            .to_string();
        let Some(repo) = self.repo else {
            return Err(refused(
                "The shared backlog isn't reachable from here yet. Try again in a minute.",
            ));
        };
        let token = match shared.links.keychain.get(&shared.links.backlog_url) {
            Ok(Some(token)) => token,
            _ => return Err(refused("This computer isn't signed in to the shared backlog.")),
        };
        // Fail closed: a sync or a merge the store couldn't be asked about is never guessed at.
        let items = match self.client.list(repo, Some(&token)).await {
            Ok(items) => items,
            Err(e) => {
                self.store_error("list", &e);
                return Err(Why::Failed(format!("Couldn't read the shared backlog: {e}")));
            }
        };
        let project = self.project.clone();
        let host = host.clone();
        let opened = tokio::task::spawn_blocking(move || open(host.as_ref(), &project, &main, &items)).await;
        match opened {
            Ok(Ok(url)) => {
                tracing::info!("{}: opened the pull request into main: {url}", self.id);
                Ok(url)
            }
            Ok(Err(why)) => Err(why),
            Err(e) => Err(Why::Failed(e.to_string())),
        }
    }
}

/// The checks and the opening, on a blocking thread (git and `gh` both wait).
fn open(host: &dyn MainPrHost, project: &ProjectConfig, main: &str, items: &[Item]) -> Result<String, Why> {
    let failed = |e: anyhow::Error| Why::Failed(format!("{e:#}"));
    let tb = host.read(project, main, items).map_err(failed)?;
    if let Some(why) = open_refusal(&tb) {
        return Err(Why::Refused(why));
    }
    if let Some(pr) = host.open_pr(project, main).map_err(failed)? {
        return Err(Why::Refused(found_open(&pr, &tb)));
    }
    let draft = host.draft(project, main).map_err(failed)?;
    host.create(project, main, &draft).map_err(failed)
}
