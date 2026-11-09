//! What the agent says about itself and its projects: the [`AgentView`] the connector sends over the
//! link and [`commands`](crate::commands) read, its [`ProjectView`]s, the [`PauseState`] they share,
//! and the [`Exit`] callback a restart runs. A leaf both `connector` and `commands` depend on, so
//! neither depends on the other for these.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::config::{GalleryConfig, ProjectConfig};
use crate::galleries;
use crate::hello_settings::HelloSettings;

mod cursor;
mod orchestrator;
mod usage;

/// The release flag that has the snapshot carry the loop's state (agent/09.2-what-the-computer-reports.md).
pub const SNAPSHOT_REPORTING_FLAG: &str = "snapshot-reporting";

/// What the hello's `capabilities` says this agent takes (agent-settings.md section 2.1).
pub const AGENT_SETTINGS_CAPABILITY: &str = "agent_settings";
use crate::lane_pool::LaneSizes;
use crate::paths;
use crate::store::Store;
use crate::text::truncate;
use crate::trial_plan::TrialState;

/// A callback that ends the process, to run restart (connector.ts's `exit`, `commands.ts`'s `restart`).
pub type Exit = dyn Fn(i32) + Send + Sync;

/// What the agent says about itself and its projects over the link.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentView {
    /// `link::machine_name`.
    pub machine: String,
    /// This computer's tags (computer-tags.md section 3.1).
    pub tags: Vec<String>,
    /// The projects, the first first; none before Setup.
    pub projects: Vec<ProjectView>,
    /// This crate's version in a checkout, or a release build's `build.json` one
    /// (installers/03-the-bundle-agent-bundle.md section 3.7).
    pub version: String,
    /// The computer's `lanes` and `choreLane`, for each project's lanes list.
    pub lanes: u32,
    pub chore_lane: Option<u32>,
    /// Each project's lanes' disk use; a change sends that project's snapshot again.
    pub lane_sizes: LaneSizes,
    /// Each paused project's reason, Pause and Resume's (orchestrator.ts's `paused`/`pausedReason`); a change
    /// sends that project's snapshot again, as a lane-size change does.
    pub paused: PauseState,
    /// Item commands (Continue) the connector's thread hands to the orchestrator loop.
    pub item_commands: ItemCommands,
    /// Each project's `work_available` wake-up for its run loop.
    pub work: crate::work_signal::WorkSignals,
    /// A pending Restart, shared with the exit that runs it (`runtime::restart_handler`).
    pub restart: Arc<crate::restart::RestartState>,
    /// The computer's settings for `hello`, and the signal that sends it again after a save.
    pub settings: HelloSettings,
    /// The computer's local model, for the snapshot's `usage.localDown`.
    pub local: crate::local_model::LocalModel,
    /// What the orchestrator loops are doing, for the snapshot's orchestrator block.
    pub live: crate::orchestrator::live::LiveHandle,
    /// The setup checks' runner (first-run-check.md section 3.1), for `hello` and commands.
    pub checks: Arc<crate::setup_check::SetupCheckRunner>,
    /// The GitHub login of this computer's `gh`: the owner `approve_commands` is checked against. Read once.
    pub owner: Arc<std::sync::OnceLock<String>>,
}

/// A command about one item, queued by [`commands`](crate::commands) for the orchestrator loop to carry out on
/// its next pass (agent/09-web-ui.md section 9.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemCommand {
    /// **Continue** on a stopped item: `notes` (with any picture paths) are the implementer's next message.
    Continue {
        project: String,
        item: String,
        notes: Option<String>,
    },
    /// **Start over** on a stopped item: the slice starts again on a new branch, `notes` in its first prompt.
    StartOver {
        project: String,
        item: String,
        notes: Option<String>,
    },
    /// A person's answer to the playtest of item `id`; `notes` carry any picture paths.
    Playtest {
        project: String,
        id: String,
        passed: bool,
        notes: Option<String>,
        /// When the item merged, for a deferred playtest's try-out key `<id>@<mergedAt>`.
        merged_at: Option<String>,
    },
    /// **Skip** on a stopped item: it's let go and skipped in the store, its branch kept.
    Skip { project: String, item: String },
    /// **Play** in lane `lane` (> 0), decided by the loop, which owns the lanes.
    Play { project: String, lane: u32 },
    /// **Stop** the game in lane `lane`.
    Stop { project: String, lane: u32 },
    /// **Restart** the game in lane `lane`: stop it if one runs, then play again.
    RestartPlay { project: String, lane: u32 },
    /// **Generate ideas** (`ideas_generate { fresh }`): one non-quiet ideas round; `fresh` drops the waiting ideas first.
    Ideas { project: String, fresh: bool },
}

impl ItemCommand {
    fn is_lane(&self) -> bool {
        matches!(self, Self::Play { .. } | Self::Stop { .. } | Self::RestartPlay { .. })
    }

    fn project(&self) -> &str {
        match self {
            Self::Continue { project, .. }
            | Self::StartOver { project, .. }
            | Self::Skip { project, .. }
            | Self::Playtest { project, .. }
            | Self::Play { project, .. }
            | Self::Stop { project, .. }
            | Self::RestartPlay { project, .. }
            | Self::Ideas { project, .. } => project,
        }
    }
}

/// A press on a draft waiting for a person (**Approve**, **Merge into main** or **Discard**), carried out whether or not the
/// project is paused (agent/06.5-spec-drafts.md step 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftCommand {
    Approve { project: String, draft: String },
    Merge { project: String, draft: String },
    Discard { project: String, draft: String },
}

/// A chat message for the assistant (`ask` command, spec 06.6), queued for the orchestrator loop to answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskCommand {
    pub project: String,
    pub ask_id: String,
    pub text: String,
    /// The ask this one replies to.
    pub reply_to: Option<String>,
    /// The item or plan whose page it was asked on.
    pub about: Option<String>,
    /// The "Attached images…" block made from the saved pictures, empty when there are none.
    pub images_block: String,
}

/// An `idea_accept` waiting for the loop, which writes the idea up as a draft and answers command `command_id` once the
/// store has (`ideas::accept`).
#[derive(Debug, Clone)]
pub struct IdeaCommand {
    pub project: String,
    pub command_id: String,
    pub idea: String,
    /// Who pressed Build this.
    pub by: String,
}

/// The queue of [`ItemCommand`]s, shared between the connector's thread and the orchestrator `Machine`
/// (held and passed like [`PauseState`]), plus whether the orchestrator is in `claim` mode, the only mode
/// that carries them out.
#[derive(Debug, Clone, Default)]
pub struct ItemCommands {
    claim: Arc<std::sync::atomic::AtomicBool>,
    queue: Arc<std::sync::Mutex<Vec<ItemCommand>>>,
    drafts: Arc<std::sync::Mutex<Vec<DraftCommand>>>,
    asks: Arc<std::sync::Mutex<Vec<AskCommand>>>,
    /// The `(project, flag)` pairs On, set by the loop each pass.
    flags_on: Arc<std::sync::Mutex<std::collections::HashSet<(String, &'static str)>>>,
    ideas: Arc<std::sync::Mutex<Vec<IdeaCommand>>>,
    /// The number auto concurrency last allowed and why; `None` while it is Off or has not stepped yet.
    auto: Arc<std::sync::Mutex<Option<(u32, String)>>>,
    /// Each open connection's outbound messages, by project, for the answer to a command the loop settles.
    out: Arc<std::sync::Mutex<std::collections::HashMap<String, tokio::sync::mpsc::UnboundedSender<Value>>>>,
    /// Each project's allowed commands (`approve-commands-gate`), kept by its loop.
    gates: crate::command_gate::CommandGates,
}

impl PartialEq for ItemCommands {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.queue, &other.queue)
    }
}

impl ItemCommands {
    /// Set once at start: whether the orchestrator really runs in `claim` mode.
    pub fn set_claim(&self, claim: bool) {
        self.claim.store(claim, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn claim(&self) -> bool {
        self.claim.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A project's `cfg` as it may run: with the allowed commands over it while `approve-commands-gate` is On for
    /// it, else as given. Everything that runs a project command takes its config through here.
    pub fn allowed_config(&self, project: &str, cfg: &ProjectConfig) -> ProjectConfig {
        if self.flag_on(project, crate::command_gate::FLAG) {
            self.gates.config(project, cfg)
        } else {
            cfg.clone()
        }
    }

    /// [`allowed_config`](Self::allowed_config) for a project as the connector holds it.
    pub fn allowed_view(&self, project: &ProjectView) -> ProjectView {
        if self.flag_on(&project.id, crate::command_gate::FLAG) {
            self.gates.view(project)
        } else {
            project.clone()
        }
    }

    pub fn gates(&self) -> &crate::command_gate::CommandGates {
        &self.gates
    }

    pub fn push(&self, command: ItemCommand) {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).push(command);
    }

    pub fn push_draft(&self, command: DraftCommand) {
        self.drafts.lock().unwrap_or_else(|e| e.into_inner()).push(command);
    }

    /// Set by the loop each pass: whether the `ask-claude` flag is On for `project`.
    pub fn set_ask_on(&self, project: &str, on: bool) {
        self.set_flag(project, "ask-claude", on);
    }

    pub fn ask_on(&self, project: &str) -> bool {
        self.flag_on(project, "ask-claude")
    }

    /// Records whether `flag` is On for `project`; commands behind a flag are refused while it is Off.
    pub fn set_flag(&self, project: &str, flag: &'static str, on: bool) {
        let mut set = self.flags_on.lock().unwrap_or_else(|e| e.into_inner());
        if on {
            set.insert((project.to_string(), flag));
        } else {
            set.remove(&(project.to_string(), flag));
        }
    }

    pub fn flag_on(&self, project: &str, flag: &'static str) -> bool {
        let set = self.flags_on.lock().unwrap_or_else(|e| e.into_inner());
        set.contains(&(project.to_string(), flag))
    }

    /// Set by the loop (`driver/auto.rs`) for the snapshot's `usage.autoConcurrency`.
    pub fn set_auto(&self, auto: Option<(u32, String)>) {
        *self.auto.lock().unwrap_or_else(|e| e.into_inner()) = auto;
    }

    pub fn auto(&self) -> Option<(u32, String)> {
        self.auto.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn push_idea(&self, command: IdeaCommand) {
        self.ideas.lock().unwrap_or_else(|e| e.into_inner()).push(command);
    }

    /// Removes and returns the `idea_accept`s queued for `project`, oldest first.
    pub fn take_ideas(&self, project: &str) -> Vec<IdeaCommand> {
        let mut queue = self.ideas.lock().unwrap_or_else(|e| e.into_inner());
        let (mine, rest) = std::mem::take(&mut *queue)
            .into_iter()
            .partition(|c| c.project == project);
        *queue = rest;
        mine
    }

    /// A connection to the hub for `project` opened (`None`: it closed): [`answer`](Self::answer) goes up it.
    pub fn set_out(&self, project: &str, out: Option<tokio::sync::mpsc::UnboundedSender<Value>>) {
        let mut map = self.out.lock().unwrap_or_else(|e| e.into_inner());
        match out {
            Some(out) => map.insert(project.to_string(), out),
            None => map.remove(project),
        };
    }

    /// Sends a command's `ack` or `nack` that was settled later, by the loop. Dropped when the connection is gone.
    pub fn answer(&self, project: &str, answer: Value) {
        if let Some(out) = self.out.lock().unwrap_or_else(|e| e.into_inner()).get(project) {
            let _ = out.send(answer);
        }
    }

    pub fn push_ask(&self, command: AskCommand) {
        self.asks.lock().unwrap_or_else(|e| e.into_inner()).push(command);
    }

    /// Removes and returns the asks queued for `project`, oldest first.
    pub fn take_asks(&self, project: &str) -> Vec<AskCommand> {
        let mut queue = self.asks.lock().unwrap_or_else(|e| e.into_inner());
        let (mine, rest) = std::mem::take(&mut *queue)
            .into_iter()
            .partition(|a| a.project == project);
        *queue = rest;
        mine
    }

    /// Removes and returns the draft presses queued for `project`, oldest first.
    pub fn take_drafts(&self, project: &str) -> Vec<DraftCommand> {
        let mut queue = self.drafts.lock().unwrap_or_else(|e| e.into_inner());
        let (mine, rest) = std::mem::take(&mut *queue).into_iter().partition(|c| match c {
            DraftCommand::Approve { project: p, .. }
            | DraftCommand::Merge { project: p, .. }
            | DraftCommand::Discard { project: p, .. } => p == project,
        });
        *queue = rest;
        mine
    }

    /// Removes and returns `project`'s queued commands, oldest first.
    pub fn take(&self, project: &str) -> Vec<ItemCommand> {
        self.take_where(project, |c| !c.is_lane())
    }

    /// Removes and returns `project`'s queued Play and Stop of a lane, oldest first.
    pub fn take_lane(&self, project: &str) -> Vec<ItemCommand> {
        self.take_where(project, ItemCommand::is_lane)
    }

    fn take_where(&self, project: &str, keep: impl Fn(&ItemCommand) -> bool) -> Vec<ItemCommand> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let (mine, rest) = std::mem::take(&mut *queue)
            .into_iter()
            .partition(|c| c.project() == project && keep(c));
        *queue = rest;
        mine
    }
}

/// A project's pause, Pause and Resume's (`{ computer: true }` only; the project-wide pause is the shared
/// backlog's own, answered by retaskable.com without reaching a computer). Keyed and watched like
/// [`LaneSizes`], since both resend a project's snapshot the same way.
#[derive(Debug, Clone)]
pub struct PauseState(Arc<watch::Sender<BTreeMap<String, String>>>);

impl Default for PauseState {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(BTreeMap::new())))
    }
}

impl PartialEq for PauseState {
    fn eq(&self, other: &Self) -> bool {
        *self.0.borrow() == *other.0.borrow()
    }
}

impl PauseState {
    pub fn reason(&self, project: &str) -> Option<String> {
        self.0.borrow().get(project).cloned()
    }

    /// `Some(reason)` pauses the project with that reason, `None` resumes it.
    pub fn set(&self, project: &str, reason: Option<String>) {
        self.0.send_if_modified(|all| match reason {
            Some(reason) => {
                if all.get(project) == Some(&reason) {
                    return false;
                }
                all.insert(project.to_string(), reason);
                true
            }
            None => all.remove(project).is_some(),
        });
    }

    pub fn subscribe(&self) -> watch::Receiver<BTreeMap<String, String>> {
        self.0.subscribe()
    }
}

/// A project as its hub knows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectView {
    pub id: String,
    pub github_repo: String,
    pub backlog_path: String,
    /// What the Shoot command needs to run `galleries::shoot` (`commands.rs`): the base checkout's
    /// own clone, where its worktrees go, which branch is the base, and the project's own galleries.
    pub repo_path: PathBuf,
    pub worktrees_dir: PathBuf,
    pub base_branch: String,
    pub galleries: Vec<GalleryConfig>,
    /// Play main's command (`commands.rs`'s `play`).
    pub play_command: String,
}

impl From<&ProjectConfig> for ProjectView {
    fn from(p: &ProjectConfig) -> Self {
        Self {
            id: p.id_or_derived(),
            github_repo: p.github_repo.clone(),
            backlog_path: p.backlog_path.clone(),
            repo_path: PathBuf::from(&p.repo_path),
            worktrees_dir: PathBuf::from(&p.worktrees_dir),
            base_branch: p.base_branch.clone(),
            galleries: p.galleries.clone(),
            play_command: p.play_command.clone(),
        }
    }
}

impl AgentView {
    /// A view with no projects, for tests that only need something to hand a host.
    #[cfg(test)]
    pub(crate) fn blank() -> Self {
        Self {
            machine: "pc".into(),
            tags: vec![],
            projects: vec![],
            version: "1".into(),
            lanes: 1,
            chore_lane: None,
            lane_sizes: Default::default(),
            paused: Default::default(),
            item_commands: Default::default(),
            work: Default::default(),
            restart: Default::default(),
            settings: Default::default(),
            local: Default::default(),
            live: Default::default(),
            checks: Default::default(),
            owner: Default::default(),
        }
    }

    /// This computer's owner (`repo::github_login`), read on first use; `None` while `gh` can't say.
    pub fn owner(&self) -> Option<String> {
        if let Some(o) = self.owner.get() {
            return Some(o.clone());
        }
        let login = crate::repo::github_login()?;
        Some(self.owner.get_or_init(|| login).clone())
    }

    /// The hubs to connect to: each project's by its id, or the agent's own ("") before the first project.
    pub(crate) fn keys(&self) -> Vec<String> {
        if self.projects.is_empty() {
            vec![String::new()]
        } else {
            self.projects.iter().map(|p| p.id.clone()).collect()
        }
    }

    /// `hello` (cloud.md section 5.3): the agent's version, OS and machine, and the project's repo and backlog, or
    /// null until Setup is done.
    pub async fn hello(&self, project: &str) -> Value {
        let repo = self
            .projects
            .iter()
            .find(|p| p.id == project && !p.github_repo.is_empty());
        let mut hello = json!({
            "type": "hello",
            "agentVersion": truncate(&self.version, 50),
            "os": platform(),
            "machine": truncate(&self.machine, 200),
            "capabilities": [],
            "policy": {},
            "project": repo.map(|p| json!({ "githubRepo": p.github_repo, "backlogPath": p.backlog_path })),
            "tags": self.tags,
        });
        if self.settings.on().await {
            hello["capabilities"] = json!([AGENT_SETTINGS_CAPABILITY]);
        }
        if let Some(settings) = self.settings.json().await {
            hello["settings"] = settings;
        }
        if self.settings.setup_checks_on().await
            && let Some(checks) = self.checks.latest()
        {
            hello["checks"] = checks.to_json();
        }
        hello
    }

    /// The project's snapshot (cloud.md section 5.2): every list the hub needs. Most are still
    /// empty, since this agent runs no work yet, but a project's galleries report their real
    /// status and last shoot (agent/09-web-ui.md section 9.2), and an orchestrator that says so.
    pub fn snapshot(&self, project: &str, data_dir: &Path) -> Value {
        let orchestrator = self.orchestrator_block(project);
        let mut state = json!({
                "orchestrator": orchestrator,
                "usage": {},
                "milestones": [],
                "items": [],
                "sessions": [],
                "lanes": self.lanes_list(project),
                "mainPlaying": self.settings.snapshot_reporting_on() && crate::play::running(project),
                "galleries": self.galleries_list(project, data_dir),
                "drafts": [],
                "asks": [],
        });
        if let Some((items, milestones)) = self.items_block(project) {
            state["items"] = items;
            state["milestones"] = milestones;
        }
        if let Some(window) = self.usage_block(project, data_dir) {
            state["usage"] = window;
        }
        if self.local.down() {
            state["usage"]["localDown"] = json!(true);
        }
        if let Some(cursor) = self.cursor_usage() {
            state["usage"]["cursor"] = cursor;
        }
        if let Some((allowed, reason)) = self.item_commands.auto() {
            state["usage"]["autoConcurrency"] = json!({ "allowed": allowed, "reason": reason });
        }
        if let Some(trial) = self.trial_json(project, data_dir) {
            state["trial"] = trial;
        }
        if let Some(smoke) = self.smoke_json(project, data_dir) {
            state["smoke"] = smoke;
        }
        json!({ "type": "snapshot", "state": state })
    }

    /// The snapshot's `items` and `milestones` (09.2): with `snapshot-reporting` On, the items this agent holds a
    /// lease on, and the Chores milestone when one is a chore.
    fn items_block(&self, project: &str) -> Option<(Value, Value)> {
        if !self.settings.snapshot_reporting_on() {
            return None;
        }
        let held = self
            .loop_state(project)
            .and_then(|s| s.projects.get(project).map(|p| p.items.clone()))
            .unwrap_or_default();
        let items: Vec<Value> = held
            .iter()
            .map(|i| {
                let mut item = json!({
                    "id": i.id, "title": i.title, "state": i.state, "branch": i.branch,
                    "attempt": i.attempt, "chore": i.chore,
                });
                if let Some(lane) = i.lane {
                    item["lane"] = json!(lane);
                }
                if let Some(pr) = &i.pr_url {
                    item["prUrl"] = json!(pr);
                }
                item
            })
            .collect();
        let milestones = if held.iter().any(|i| i.chore) {
            json!([{ "id": "chores", "title": "Chores" }])
        } else {
            json!([])
        };
        Some((json!(items), milestones))
    }

    /// The snapshot's `smoke` (09.2): the project's `state.smoke` as saved, with `snapshot-reporting` On and a
    /// smoke to report. `tail` is kept only for a failure or timeout.
    pub fn smoke_json(&self, project: &str, data_dir: &Path) -> Option<Value> {
        use crate::store::SmokeStatus;
        if !self.settings.snapshot_reporting_on() || !self.projects.iter().any(|p| p.id == project) {
            return None;
        }
        let mut smoke = Store::load(&paths::project_dir(data_dir, project)).ok()?.smoke()?;
        if !matches!(smoke.status, SmokeStatus::Failed | SmokeStatus::Timeout) {
            smoke.tail = None;
        }
        Some(json!(smoke))
    }

    /// The snapshot's `trial` (first-run-check.md section 3.3, "Reporting it"): the project's trial
    /// record from its `state.json`, none before Setup. A skipped record reports as not started.
    pub fn trial_json(&self, project: &str, data_dir: &Path) -> Option<Value> {
        self.projects.iter().find(|p| p.id == project)?;
        let record = Store::load(&paths::project_dir(data_dir, project))
            .map(|s| s.trial())
            .unwrap_or_default();
        let state = if record.skipped {
            TrialState::NotStarted
        } else {
            record.state
        };
        let mut trial = json!({ "state": state });
        if state == TrialState::Open
            && let Some(url) = record.pr_url
        {
            trial["prUrl"] = json!(url);
        }
        Some(trial)
    }

    /// The snapshot's galleries (tray/05.3-private-galleries-and-pictures-from.md section 5.3.1):
    /// each of the project's own galleries' [`galleries::GalleryInfo`], none before Setup.
    fn galleries_list(&self, project: &str, data_dir: &Path) -> Vec<Value> {
        self.projects
            .iter()
            .find(|p| p.id == project)
            .map(|p| {
                p.galleries
                    .iter()
                    .map(|g| json!(galleries::info(data_dir, project, g)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The snapshot's lanes (pageView.ts): each of the computer's lanes with its disk use, none holding work yet.
    fn lanes_list(&self, project: &str) -> Vec<Value> {
        // With `snapshot-reporting` On a lane shows the item it holds, and a lane held past `lanes` (lowered since)
        // stays listed until it finishes (pageView.ts).
        let held = self
            .loop_state(project)
            .and_then(|s| s.projects.get(project).map(|p| p.held.clone()))
            .unwrap_or_default();
        let top = held.iter().map(|h| h.lane).max().unwrap_or(0).max(self.lanes);
        (1..=top)
            .filter(|k| *k <= self.lanes || held.iter().any(|h| h.lane == *k))
            .map(|k| {
                let mut lane = json!({ "k": k, "diskGb": self.lane_sizes.gb(project, k), "playing": false });
                if let Some(h) = held.iter().find(|h| h.lane == k) {
                    lane["itemId"] = json!(h.item_id);
                    lane["playing"] = json!(h.playing);
                    if let Some(branch) = &h.branch {
                        lane["branch"] = json!(branch);
                    }
                }
                if Some(k) == self.chore_lane {
                    lane["note"] = json!(CHORE_LANE_NOTE);
                }
                lane
            })
            .collect()
    }
}

/// The chore lane's note in the lanes list (pageView.ts).
pub const CHORE_LANE_NOTE: &str = "chore lane: playtest fixes first, then roadmap items, then chores";

/// Why a project's orchestrator reads as paused.
pub const NO_WORK_YET: &str = "This computer's agent doesn't run work yet.";

/// `process.platform`'s name for this OS, as the hub keeps it.
fn platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

#[cfg(test)]
mod tests;
