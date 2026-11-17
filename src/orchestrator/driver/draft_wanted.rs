//! A person's Add spec and Build this on a project's loop (docs/specs/intent-rows.md section 4, `draft_wanted`; the
//! handler is `intents/draft_wanted.rs`), and the site's reads (`read_file`, `issues_preview`: the rows, `intents/reads.rs`,
//! and the pushed commands the hub still sends, `commands/reads.rs`): at most every minute (at once on the store socket's
//! `facts_changed` naming `intents`) the intent puller lists the project's rows and takes each one, in `claim` and in
//! `runs` mode alike. A paused project, a computer signed out of Claude and a
//! computer already running two rows that start work take none: the row stays open for another computer. No session
//! slot is used here; the draft is queued in the store, and a drafter claims it from there.
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::project::ProjectLoop;
use super::signed_out::signed_out;
use super::{Links, Shared};
use crate::ask::pictures;
use crate::intents::draft_wanted::{RowCx, WantedHandler};
use crate::intents::reads::ReadHandler;
use crate::intents::reads::has_deploy;
use crate::intents::{Gates, IntentStore, Puller, StoreLink, Work};
use crate::kept_facts;
use crate::read_kinds;
use crate::view::ReadCommand;
use crate::work_signal::Wake;

/// How often the store is asked for them (docs/specs/intent-rows.md section 3.3).
const POLL_EVERY: Duration = Duration::from_secs(60);

/// A project's intent rows: the puller (built the first time a pass runs) and when the store was last asked.
pub(super) struct WantedRows {
    puller: Option<Puller<RowCx>>,
    polled: Option<Instant>,
    /// Whether the project has a `deploy` section, which an `environments` row needs of the computer that claims it.
    deploy: Arc<std::sync::atomic::AtomicBool>,
    /// How long after one look at the store the next may be.
    pub(super) poll_every: Duration,
}

impl Default for WantedRows {
    fn default() -> Self {
        Self {
            puller: None,
            polled: None,
            deploy: Default::default(),
            poll_every: POLL_EVERY,
        }
    }
}

impl ProjectLoop {
    /// Takes the `draft_wanted` rows the store lists for this project that nothing holds back. `paused` is this
    /// computer's or the store's pause; `work` counts the rows that start work running on this computer.
    pub(super) async fn wanted_pass(
        &mut self,
        paused: bool,
        work: &Work,
        links: &Links,
        stopping: bool,
        shared: &Shared<'_>,
    ) {
        if stopping {
            return;
        }
        let Some(repo) = self.repo else { return };
        // A push naming the intents: the store has a row now, so ask at once.
        if links.work.take(&self.id, Wake::Intents) {
            self.wanted.polled = None;
        }
        if self
            .wanted
            .polled
            .is_some_and(|at| at.elapsed() < self.wanted.poll_every)
        {
            return;
        }
        let token = match links.store_token().await {
            Ok(Some(token)) => token,
            Ok(None) => return,
            Err(e) => return self.cant_start(format!("reading the backlog link: {e:#}")),
        };
        self.wanted.polled = Some(Instant::now());
        let link: Arc<dyn IntentStore> = Arc::new(StoreLink {
            client: self.client.clone(),
            repo,
            token: token.clone(),
            machine: links.machine.clone(),
            failures: self.failures.clone(),
        });
        let gates = Gates {
            paused,
            signed_out: signed_out(shared),
            work: work.clone(),
        };
        let deploy = self.wanted.deploy.clone();
        deploy.store(has_deploy(&self.project.deploy), std::sync::atomic::Ordering::SeqCst);
        let puller = (self.wanted.puller).get_or_insert_with(|| {
            let puller = Puller::default()
                .keeping(self.store.dir())
                .register(Arc::new(WantedHandler));
            read_kinds::KINDS.iter().fold(puller, |p, kind| {
                let flag = (*kind == "environments").then(|| deploy.clone());
                p.register(Arc::new(ReadHandler(kind, flag)))
            })
        });
        let facts = kept_facts::shared(self.store.dir());
        let deploy_log = crate::promote::DeployLog::shared(&self.store.dir().join(crate::promote::log::FILE));
        let (client, project) = (self.client.clone(), self.project.clone());
        let cx = move || RowCx {
            client,
            repo,
            token,
            pictures: pictures::folder(),
            project,
            facts,
            deploy_log: Some(deploy_log),
        };
        puller.pass_gated(&link, &gates, cx).await;
    }
}

impl ProjectLoop {
    /// Runs the reads the hub pushed (`read_file`, `issues_preview`) since the last pass, each in a task of its own that
    /// answers the command when it is done: a slow `gh` holds nothing up. Fails closed: with the store not reachable
    /// (no link, no token) the command is refused, never answered from a guess.
    pub(super) async fn read_pass(&mut self, links: &Links) {
        for ReadCommand {
            command_id, kind, args, ..
        } in links.item_commands.take_reads(&self.id)
        {
            let reply = |msg: serde_json::Value| links.item_commands.answer(&self.id, msg);
            let refuse = |why: String| reply(json!({ "type": "nack", "id": command_id, "reason": why }));
            let Some(repo) = self.repo else {
                refuse("This project isn't linked to the shared backlog.".into());
                continue;
            };
            let token = match links.store_token().await {
                Ok(Some(token)) => token,
                Ok(None) => {
                    refuse("This computer isn't linked to the shared backlog.".into());
                    continue;
                }
                Err(e) => {
                    refuse(format!("Can't reach the shared backlog: {e:#}"));
                    continue;
                }
            };
            let reading = read_kinds::Reading {
                client: self.client.clone(),
                repo,
                token,
                project: self.project.clone(),
                gh: "gh".into(),
                deploy_log: Some(crate::promote::DeployLog::shared(
                    &self.store.dir().join(crate::promote::log::FILE),
                )),
            };
            let (items, project) = (links.item_commands.clone(), self.id.clone());
            tokio::spawn(async move {
                let msg = match read_kinds::read(&kind, &args, &reading).await {
                    Ok(result) => json!({ "type": "ack", "id": command_id, "result": result }),
                    Err(e) => json!({ "type": "nack", "id": command_id, "reason": e.why() }),
                };
                items.answer(&project, msg);
            });
        }
    }
}
