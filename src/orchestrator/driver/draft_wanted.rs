//! A person's Add spec and Build this on a project's loop (docs/specs/intent-rows.md section 4, `draft_wanted`; the
//! handler is `intents/draft_wanted.rs`), the design review's presses (`design_skip_requested`, `design_draw_requested`:
//! `intents/design.rs`, handed to the draft step, which runs them) and the site's reads (`read_file`, `issues_preview`,
//! `environments`, `draft_files`, `release_status`: the rows, `intents/reads.rs`; the hub still pushes `release_status`
//! and `draft_files`, run by `read_pass` below): at most every minute (at once on the store socket's
//! `facts_changed` naming `intents`) the intent puller lists the project's rows, tells the store why this computer
//! takes nothing (`intent_seen`) and takes each row, in `claim` and in `runs` mode alike. A paused project, a computer signed out of Claude and a
//! computer already running two rows that start work take none: the row stays open for another computer. No session
//! slot is used here; the draft is queued in the store, and a drafter claims it from there.
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::mpsc;

use super::project::ProjectLoop;
use super::signed_out::{out_of_time, signed_out};
use super::{Links, Shared};
use crate::ask::pictures;
use crate::intents::design::{DesignHandler, Press};
use crate::intents::draft_wanted::{RowCx, WantedHandler};
use crate::intents::reads::{Holds, ReadHandler, has_deploy};
use crate::intents::{Gates, IntentStore, Puller, StoreLink, Work};
use crate::kept_facts;
use crate::read_kinds;
use crate::view::{DraftCommand, ReadCommand, Reply};
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
    /// The design presses the rows' handlers hand over, and where the loop takes them.
    presses: mpsc::UnboundedSender<Press>,
    pressing: mpsc::UnboundedReceiver<Press>,
}

impl Default for WantedRows {
    fn default() -> Self {
        let (presses, pressing) = mpsc::unbounded_channel();
        Self {
            puller: None,
            polled: None,
            deploy: Default::default(),
            poll_every: POLL_EVERY,
            presses,
            pressing,
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
        // The design presses the handlers handed over go to the draft step, which runs them and answers each row.
        while let Ok(Press {
            draft,
            note,
            draw,
            reply,
        }) = self.wanted.pressing.try_recv()
        {
            let (project, reply) = (self.id.clone(), Reply::new(reply));
            links.item_commands.push_draft(if draw {
                DraftCommand::DrawDesign {
                    project,
                    draft,
                    note,
                    reply,
                }
            } else {
                DraftCommand::SkipDesign { project, draft, reply }
            });
        }
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
            out_of_time: out_of_time(shared),
            report: true,
            work: work.clone(),
        };
        let deploy = self.wanted.deploy.clone();
        deploy.store(has_deploy(&self.project.deploy), std::sync::atomic::Ordering::SeqCst);
        let facts = kept_facts::shared(self.store.dir());
        let (worktrees, held) = (std::path::PathBuf::from(&self.project.worktrees_dir), facts.clone());
        let presses = self.wanted.presses.clone();
        let puller = (self.wanted.puller).get_or_insert_with(|| {
            let holds: Holds = Arc::new(move |id| holds_plan(&worktrees, &held, id));
            let puller = Puller::default()
                .keeping(self.store.dir())
                .register(Arc::new(WantedHandler))
                .register(Arc::new(DesignHandler::skip(presses.clone())))
                .register(Arc::new(DesignHandler::draw(presses)));
            read_kinds::KINDS.iter().fold(puller, |p, kind| {
                let read = ReadHandler::new(kind);
                let read = match *kind {
                    "environments" => read.needing_deploy(deploy.clone()),
                    "draft_files" => read.holding(holds.clone()),
                    _ => read,
                };
                p.register(Arc::new(read))
            })
        });
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

/// Whether this computer holds plan `id`: its worktree (`draft-<id>`) is here, or the store is owed its files. Only such a
/// computer answers a `draft_files` row, as only the TS agent that had the draft in its store did.
fn holds_plan(worktrees: &std::path::Path, facts: &kept_facts::KeptFacts, id: &str) -> bool {
    !id.is_empty()
        && !id.contains(['/', '\\'])
        && !id.contains("..")
        && (crate::lanes::is_worktree(&crate::draft_worktree::path(worktrees, id))
            || facts.owed(&format!("draft_files/{id}")).is_some())
}

impl ProjectLoop {
    /// Runs the reads the hub still pushes (`release_status`) since the last pass, each in a task of its own that
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
                deploy_log: None,
                facts: Some(crate::kept_facts::shared(self.store.dir())),
            };
            let (items, project) = (links.item_commands.clone(), self.id.clone());
            tokio::spawn(async move {
                let msg = match read_kinds::read(&kind, &args, &reading).await {
                    // A command that answers nothing is acked as TS acks it: with no `result`.
                    Ok(serde_json::Value::Null) => json!({ "type": "ack", "id": command_id }),
                    Ok(result) => json!({ "type": "ack", "id": command_id, "result": result }),
                    Err(e) => json!({ "type": "nack", "id": command_id, "reason": e.why() }),
                };
                items.answer(&project, msg);
            });
        }
    }
}
