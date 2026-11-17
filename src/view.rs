//! What the agent says about itself and its projects: the [`AgentView`] the connector sends over the
//! link and [`commands`](crate::commands) read, its [`ProjectView`]s, the [`PauseState`] they share,
//! and the [`Exit`] callback a restart runs. A leaf both `connector` and `commands` depend on, so
//! neither depends on the other for these.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::config::ProjectConfig;
use crate::hello_settings::HelloSettings;

mod cloud_link;
mod cursor;
mod orchestrator;
mod project_commands;
pub use cloud_link::{CloudLinkSlot, CloudToken, Link};
pub use project_commands::{ProjectCommand, ProjectCommands};
pub(crate) mod usage;

use crate::lane_pool::LaneSizes;
use crate::text::truncate;

/// A callback that ends the process, to run restart (connector.ts's `exit`, `commands.ts`'s `restart`).
pub type Exit = dyn Fn(i32) + Send + Sync;

/// What the agent says about itself and its projects over the link.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentView {
    /// `link::machine_name`.
    pub machine: String,
    /// This computer's tags (computer-tags.md section 3.1).
    pub tags: crate::tags::LiveTags,
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
    /// Each project's pause as the shared backlog's store says it (`obey-shared-pause`); a change sends that
    /// project's snapshot again.
    pub store_paused: StorePauseState,
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
    /// Install and Sign in on Getting started (`install_tool`, `tool_sign_in`); `None` in a view that doesn't run them.
    pub setup: Option<Arc<crate::setup_tools::Setup>>,
    /// The owners and repos `gh` lists for Link project… (`github_owners`, `github_repos`).
    pub github: Arc<crate::github_pick::GithubPicker>,
    /// Starts this computer's link to the shared backlog (`backlog_link`, Link again); empty until the agent has made its links.
    pub backlog_link: BacklogLinker,
    /// Adds a project (`link_project`, the settings page's Add project); empty until the agent has made it.
    pub add_project: AddProjectSlot,
}

/// The agent's one Add project flow ([`crate::add_project::AddProject`]), filled in once the agent has made it.
#[derive(Clone, Default)]
pub struct AddProjectSlot(Arc<std::sync::OnceLock<Arc<crate::add_project::AddProject>>>);

impl AddProjectSlot {
    pub fn set(&self, add: Arc<crate::add_project::AddProject>) {
        let _ = self.0.set(add);
    }

    pub fn get(&self) -> Option<Arc<crate::add_project::AddProject>> {
        self.0.get().cloned()
    }
}

impl std::fmt::Debug for AddProjectSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AddProjectSlot")
    }
}

impl PartialEq for AddProjectSlot {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// What `backlog_link` starts: the shared backlog's device flow, in the running agent's own links
/// ([`crate::link::pending::Links`]), against the store's address. Filled in once the agent has made them.
#[derive(Clone, Default)]
pub struct BacklogLinker(Arc<std::sync::OnceLock<(crate::link::pending::Links, String)>>);

impl BacklogLinker {
    pub fn set(&self, links: crate::link::pending::Links, store_url: String) {
        let _ = self.0.set((links, store_url));
    }

    /// Starts the device flow, opening the approval page here as the tray does; one already waiting is left to wait.
    /// `Err` when there are no links to start it with.
    pub fn start(&self) -> Result<(), String> {
        let Some((links, url)) = self.0.get() else {
            return Err("This agent can't link to the shared backlog from here.".into());
        };
        links.start_flow(
            url,
            crate::link::BACKLOG_TARGET,
            crate::link::pending::StartOpts {
                open_browser: true,
                then_backlog: None,
            },
        );
        Ok(())
    }
}

impl std::fmt::Debug for BacklogLinker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BacklogLinker")
    }
}

impl PartialEq for BacklogLinker {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// The pictures a command carries, by the ids the store gave them (`pictureIds`): the loop fetches them and adds their
/// paths after the command's notes (`orchestrator::driver::attached`), then answers command `command_id`, which went
/// up as `pending`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attached {
    pub command_id: String,
    pub ids: Vec<String>,
}

/// A command about one item, queued by [`commands`](crate::commands) for the orchestrator loop to carry out on
/// its next pass (agent/09-web-ui.md section 9.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemCommand {
    /// **Continue** on a stopped item: `notes` (with the paths of `attached` pictures, once the loop has them) are the
    /// implementer's next message.
    Continue {
        project: String,
        item: String,
        notes: Option<String>,
        attached: Option<Attached>,
    },
    /// **Start over** on a stopped item: the slice starts again on a new branch, `notes` in its first prompt.
    StartOver {
        project: String,
        item: String,
        notes: Option<String>,
        attached: Option<Attached>,
    },
    /// A person's answers to the questions the session asked about `item`, in order; the paths of `attached` pictures go
    /// after the last.
    Answer {
        project: String,
        item: String,
        answers: Vec<String>,
        attached: Option<Attached>,
    },
    /// A person's answer to the playtest of item `id`; `notes` carry the paths of any `attached` pictures.
    Playtest {
        project: String,
        id: String,
        passed: bool,
        notes: Option<String>,
        attached: Option<Attached>,
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
    /// **Play this build** (`play { itemId }`): item `item`'s build, in its lane or in the project's play checkout. The loop
    /// answers command `command_id` once the game has started, or with why not.
    PlayItem {
        project: String,
        item: String,
        command_id: String,
    },
    /// **Stop** the game of item `item`'s build (`stop_play { itemId }`).
    StopItem {
        project: String,
        item: String,
        command_id: String,
    },
    /// **Restart** item `item`'s build (`restart_play { itemId }`): stop its game if one runs, then play it again.
    RestartItem {
        project: String,
        item: String,
        command_id: String,
    },
}

impl ItemCommand {
    fn is_lane(&self) -> bool {
        matches!(
            self,
            Self::Play { .. }
                | Self::Stop { .. }
                | Self::RestartPlay { .. }
                | Self::PlayItem { .. }
                | Self::StopItem { .. }
                | Self::RestartItem { .. }
        )
    }

    fn is_playtest(&self) -> bool {
        matches!(self, Self::Playtest { .. })
    }

    fn project(&self) -> &str {
        match self {
            Self::Continue { project, .. }
            | Self::StartOver { project, .. }
            | Self::Skip { project, .. }
            | Self::Answer { project, .. }
            | Self::Playtest { project, .. }
            | Self::Play { project, .. }
            | Self::Stop { project, .. }
            | Self::RestartPlay { project, .. }
            | Self::PlayItem { project, .. }
            | Self::StopItem { project, .. }
            | Self::RestartItem { project, .. } => project,
        }
    }
}

/// A press on a draft waiting for a person (**Approve**, **Merge into main** or **Discard**), carried out whether or not the
/// project is paused (agent/06.5-spec-drafts.md step 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftCommand {
    Approve {
        project: String,
        draft: String,
    },
    Merge {
        project: String,
        draft: String,
    },
    /// A `merge_requested` row's **Merge into main**: the same press as `Merge`, with the row waiting on its answer.
    MergeFor {
        project: String,
        draft: String,
        reply: Reply,
    },
    Discard {
        project: String,
        draft: String,
    },
    /// **Skip the design** (`draft_skip_design`): the plan's design review ends and **Approve plan** shows. The loop answers
    /// command `command_id` when the store has.
    SkipDesign {
        project: String,
        draft: String,
        command_id: String,
    },
    /// `board_design_write`: the plan's designs are drawn (again, with the person's `note`). The loop answers command
    /// `command_id` once they are, or with why not.
    DrawDesign {
        project: String,
        draft: String,
        note: Option<String>,
        command_id: String,
    },
}

impl DraftCommand {
    fn project(&self) -> &str {
        match self {
            Self::Approve { project, .. }
            | Self::Merge { project, .. }
            | Self::MergeFor { project, .. }
            | Self::Discard { project, .. }
            | Self::SkipDesign { project, .. }
            | Self::DrawDesign { project, .. } => project,
        }
    }

    /// A press on a plan's design review, not on the plan itself.
    pub fn is_design(&self) -> bool {
        matches!(self, Self::SkipDesign { .. } | Self::DrawDesign { .. })
    }
}

/// Where a press's answer goes: `Ok` once the draft merged, or why it didn't. Answered once; a press dropped before it
/// was answered (the loop stopping) leaves its waiter with no answer.
#[derive(Debug, Clone)]
pub struct Reply(Arc<std::sync::Mutex<Option<Sender>>>);

type Sender = tokio::sync::oneshot::Sender<Result<(), String>>;

impl Reply {
    pub fn new(sender: Sender) -> Self {
        Self(Arc::new(std::sync::Mutex::new(Some(sender))))
    }

    /// Answers, the first time only.
    pub fn send(&self, answer: Result<(), String>) {
        if let Some(sender) = self.0.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = sender.send(answer);
        }
    }
}

/// Two replies are equal when they are the same one.
impl PartialEq for Reply {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for Reply {}

/// What an expert command (`expert_start`, `expert_say`, `expert_stop`, `expert_end`) asks of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpertAction {
    Start {
        request: String,
        /// A Claude Code session of this computer to carry on (`continue`), by its id.
        continue_from: Option<String>,
    },
    Say {
        item: String,
        text: String,
        pictures: Vec<String>,
        now: bool,
    },
    Stop {
        item: String,
    },
    /// `outcome`: `park` or `discard`; the others are refused by the loop.
    End {
        item: String,
        outcome: String,
    },
    /// `expert_helper`: a helper for the session, with a lane of its own when `lane`.
    Helper {
        item: String,
        task: String,
        lane: bool,
    },
    /// `session_history` (read only): the session's past turns, `before` a time in milliseconds, up to `limit` steps.
    History {
        item: String,
        before: Option<i64>,
        limit: Option<usize>,
    },
}

/// An expert command waiting for the loop, which checks that `by` may drive the session, carries it out and answers
/// command `command_id` (`orchestrator::driver::experts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpertCommand {
    pub project: String,
    pub command_id: String,
    /// The login of whoever sent it.
    pub by: String,
    pub action: ExpertAction,
}

/// A read the site still sends as a command (`release_status`), waiting for the loop, which has the store link it needs, runs
/// it and answers command `command_id` with the answer as the `ack`'s `result` (`orchestrator::driver::reads`).
#[derive(Debug, Clone, PartialEq)]
pub struct ReadCommand {
    pub project: String,
    pub command_id: String,
    pub kind: String,
    pub args: Value,
}

/// The queue of [`ItemCommand`]s, shared between the connector's thread and the orchestrator `Machine`
/// (held and passed like [`PauseState`]), plus whether the orchestrator is in `claim` mode, the only mode
/// that carries them out.
#[derive(Debug, Clone, Default)]
pub struct ItemCommands {
    claim: Arc<std::sync::atomic::AtomicBool>,
    runs: Arc<std::sync::atomic::AtomicBool>,
    queue: Arc<std::sync::Mutex<Vec<ItemCommand>>>,
    drafts: Arc<std::sync::Mutex<Vec<DraftCommand>>>,
    experts: Arc<std::sync::Mutex<Vec<ExpertCommand>>>,
    reads: Arc<std::sync::Mutex<Vec<ReadCommand>>>,
    /// The commands on a project as a whole (`unlink_project`, the release notes' page).
    pub projects: ProjectCommands,
    /// The agent's own cloud, which only it may publish release notes to.
    pub cloud: CloudLinkSlot,
    /// Each open connection's outbound messages, by project, for the answer to a command the loop settles.
    out: Arc<std::sync::Mutex<std::collections::HashMap<String, tokio::sync::mpsc::UnboundedSender<Value>>>>,
    /// Each project's allowed commands, kept by its loop.
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

    /// Set once at start: whether the orchestrator really runs in `runs` mode.
    pub fn set_runs(&self, runs: bool) {
        self.runs.store(runs, std::sync::atomic::Ordering::SeqCst);
    }

    /// Whether the orchestrator runs the passes that aren't about an item (experts, ideas, drafts): `claim` or `runs`.
    /// The item commands stay [`claim`](Self::claim) only; lane play, stop and restart take either.
    pub fn acts(&self) -> bool {
        self.claim() || self.runs.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// A project's `cfg` as it may run: with the allowed commands over it. Everything that runs a project
    /// command takes its config through here.
    pub fn allowed_config(&self, project: &str, cfg: &ProjectConfig) -> ProjectConfig {
        self.gates.config(project, cfg)
    }

    /// [`allowed_config`](Self::allowed_config) for a project as the connector holds it.
    pub fn allowed_view(&self, project: &ProjectView) -> ProjectView {
        self.gates.view(project)
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

    pub fn push_read(&self, command: ReadCommand) {
        self.reads.lock().unwrap_or_else(|e| e.into_inner()).push(command);
    }

    /// Removes and returns the reads queued for `project`, oldest first.
    pub fn take_reads(&self, project: &str) -> Vec<ReadCommand> {
        let mut queue = self.reads.lock().unwrap_or_else(|e| e.into_inner());
        let (mine, rest) = std::mem::take(&mut *queue)
            .into_iter()
            .partition(|c| c.project == project);
        *queue = rest;
        mine
    }

    pub fn push_expert(&self, command: ExpertCommand) {
        self.experts.lock().unwrap_or_else(|e| e.into_inner()).push(command);
    }

    /// Removes and returns the expert commands queued for `project`, oldest first.
    pub fn take_experts(&self, project: &str) -> Vec<ExpertCommand> {
        let mut queue = self.experts.lock().unwrap_or_else(|e| e.into_inner());
        let (mine, rest) = std::mem::take(&mut *queue)
            .into_iter()
            .partition(|c| c.project == project);
        *queue = rest;
        mine
    }

    /// Removes and returns the draft presses queued for `project`, oldest first.
    pub fn take_drafts(&self, project: &str) -> Vec<DraftCommand> {
        let mut queue = self.drafts.lock().unwrap_or_else(|e| e.into_inner());
        let (mine, rest) = std::mem::take(&mut *queue)
            .into_iter()
            .partition(|c| c.project() == project);
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

    /// Removes and returns `project`'s queued playtest answers, oldest first: what the `runs` loop takes, where no
    /// other item command runs ([`take`](Self::take) is the `claim` loop's).
    pub fn take_playtests(&self, project: &str) -> Vec<ItemCommand> {
        self.take_where(project, ItemCommand::is_playtest)
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
pub struct PauseState(
    Arc<watch::Sender<BTreeMap<String, String>>>,
    /// Where the paused set is kept, so a restart comes up as it was left; `None` keeps it in memory only.
    Option<Arc<std::path::PathBuf>>,
);

impl Default for PauseState {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(BTreeMap::new())), None)
    }
}

impl PartialEq for PauseState {
    fn eq(&self, other: &Self) -> bool {
        *self.0.borrow() == *other.0.borrow()
    }
}

impl PauseState {
    /// The pause kept in `file` (`{ "projects": { <id>: <reason> } }`): what it holds is paused from the start, and every
    /// change is written back (atomically), so this computer's pause lasts until a person resumes it, across restarts.
    /// A file that can't be read is logged and starts nothing paused; one that can't be written is logged.
    pub fn persisted(file: &std::path::Path) -> Self {
        let held: BTreeMap<String, String> = match std::fs::read_to_string(file) {
            Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| serde_json::from_value(v.get("projects")?.clone()).ok())
                .unwrap_or_else(|| {
                    tracing::error!("Couldn't read {}: starting with nothing paused.", file.display());
                    BTreeMap::new()
                }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                tracing::error!("Couldn't read {}: {e}. Starting with nothing paused.", file.display());
                BTreeMap::new()
            }
        };
        Self(Arc::new(watch::Sender::new(held)), Some(Arc::new(file.to_path_buf())))
    }

    fn save(&self) {
        let Some(file) = &self.1 else { return };
        let all = self.0.borrow().clone();
        if let Err(e) = crate::fs_util::write_json_atomic(file, &serde_json::json!({ "projects": all }), false) {
            tracing::error!(
                "Couldn't save {}: {e:#}. A restart will forget this pause.",
                file.display()
            );
        }
    }

    pub fn reason(&self, project: &str) -> Option<String> {
        self.0.borrow().get(project).cloned()
    }

    /// `Some(reason)` pauses the project with that reason, `None` resumes it.
    pub fn set(&self, project: &str, reason: Option<String>) {
        let changed = self.0.send_if_modified(|all| match reason {
            Some(reason) => {
                if all.get(project) == Some(&reason) {
                    return false;
                }
                all.insert(project.to_string(), reason);
                true
            }
            None => all.remove(project).is_some(),
        });
        if changed {
            self.save();
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<BTreeMap<String, String>> {
        self.0.subscribe()
    }
}

/// What the store says about a project's own pause.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum StorePause {
    NotPaused,
    /// Paused, in the store's own words.
    Paused(String),
    /// The store wasn't reached, or wasn't asked.
    #[default]
    Unknown,
}

/// Each project's [`StorePause`], kept and watched like [`PauseState`]: a change resends the project's snapshot.
/// A project with nothing recorded is [`StorePause::Unknown`].
#[derive(Debug, Clone)]
pub struct StorePauseState(Arc<watch::Sender<BTreeMap<String, StorePause>>>);

impl Default for StorePauseState {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(BTreeMap::new())))
    }
}

impl PartialEq for StorePauseState {
    fn eq(&self, other: &Self) -> bool {
        *self.0.borrow() == *other.0.borrow()
    }
}

impl StorePauseState {
    pub fn get(&self, project: &str) -> StorePause {
        self.0.borrow().get(project).cloned().unwrap_or_default()
    }

    pub fn set(&self, project: &str, pause: StorePause) {
        self.0.send_if_modified(|all| {
            if pause == StorePause::Unknown {
                return all.remove(project).is_some();
            }
            if all.get(project) == Some(&pause) {
                return false;
            }
            all.insert(project.to_string(), pause);
            true
        });
    }

    pub fn subscribe(&self) -> watch::Receiver<BTreeMap<String, StorePause>> {
        self.0.subscribe()
    }
}

/// A project as its hub knows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectView {
    pub id: String,
    pub github_repo: String,
    pub backlog_path: String,
    /// The base checkout's own clone, where its worktrees go, and which branch is the base.
    pub repo_path: PathBuf,
    pub worktrees_dir: PathBuf,
    pub base_branch: String,
    /// Play main's command (`commands.rs`'s `play`).
    pub play_command: String,
    /// Play on phone's build (`phone_run`), as allowed.
    pub phone: Option<crate::config::PhoneConfig>,
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
            play_command: p.play_command.clone(),
            phone: p.phone.clone(),
        }
    }
}

impl AgentView {
    /// A view with no projects, for tests that only need something to hand a host.
    #[cfg(test)]
    pub(crate) fn blank() -> Self {
        Self {
            machine: "pc".into(),
            tags: Default::default(),
            projects: vec![],
            version: "1".into(),
            lanes: 1,
            chore_lane: None,
            lane_sizes: Default::default(),
            paused: Default::default(),
            store_paused: Default::default(),
            item_commands: Default::default(),
            work: Default::default(),
            restart: Default::default(),
            settings: Default::default(),
            local: Default::default(),
            live: Default::default(),
            checks: Default::default(),
            owner: Default::default(),
            setup: None,
            github: Default::default(),
            backlog_link: Default::default(),
            add_project: Default::default(),
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
            // None: the TypeScript agent lists `chat_context` (its `ask` takes `context`) and `peer_code`; this coding agent's `ask` is the Chat Agent's and it has no peer code (agent-settings.md section 2.1).
            "capabilities": [],
            "policy": {},
            "project": repo.map(|p| json!({ "githubRepo": p.github_repo, "backlogPath": p.backlog_path })),
            "tags": self.tags.get(),
        });
        if let Some(r) = self.settings.releases(project) {
            hello["policy"] = json!({ "releases": r.on });
        }
        if let Some(computer) = self.settings.computer(project) {
            hello["computer"] = computer;
        }
        if let Some(checks) = self.checks.latest() {
            hello["checks"] = checks.to_json();
        }
        // Sent even when empty, so the page knows this agent installs (setup-in-app.md section 5.4).
        if let Some(setup) = &self.setup {
            hello["setup"] = setup.activity();
        }
        hello
    }

    /// The project's snapshot (cloud.md section 5.2): every list the hub needs. Most are still
    /// empty, since this agent runs no work yet, and an orchestrator that says so.
    pub fn snapshot(&self, project: &str, data_dir: &Path) -> Value {
        let orchestrator = self.orchestrator_block(project);
        let mut state = json!({
                "orchestrator": orchestrator,
                "usage": {},
                "milestones": [],
                "items": [],
                "sessions": [],
                "lanes": self.lanes_list(project),
                "mainPlaying": crate::play::running(project),
                "drafts": [],
                "asks": [],
        });
        if let Some(lane) = self.loop_state(project).and_then(|s| s.added_lane) {
            state["addedLane"] = json!(lane);
        }
        if let Some(item) = self
            .loop_state(project)
            .and_then(|s| s.projects.get(project)?.playing_item.clone())
        {
            state["playingItem"] = json!(item);
        }
        // The hub finds the project's backlog in the store by this (storeKeyOf): without it every store command from
        // the site is refused with "Reading the shared backlog".
        if let Some(id) = self.loop_state(project).and_then(|s| s.projects.get(project)?.repo_id) {
            state["repoId"] = json!(id);
        }
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
        if let Some(waiting) = self.commands_waiting_json(project) {
            state["commandsWaiting"] = waiting;
        }
        if let Some(phone) = self
            .projects
            .iter()
            .find(|p| p.id == project)
            .and_then(|p| crate::phone_run::view_json(&self.item_commands.allowed_view(p), data_dir))
        {
            state["phone"] = phone;
        }
        json!({ "type": "snapshot", "state": state })
    }

    /// The snapshot's `commandsWaiting`: the version of a shared project's commands that waits for the owner's
    /// approval, with the text to show. The values are never sent.
    pub fn commands_waiting_json(&self, project: &str) -> Option<Value> {
        let w = self.item_commands.gates().waiting(project)?;
        Some(json!({ "version": w.version, "text": w.text, "by": w.by }))
    }

    /// The snapshot's `items` and `milestones` (09.2): the items this agent holds a
    /// lease on, and the Chores milestone when one is a chore.
    fn items_block(&self, project: &str) -> Option<(Value, Value)> {
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
                if let Some(expert) = &i.expert {
                    item["expert"] = expert.clone();
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
mod hub_tests;
#[cfg(test)]
mod tests;
