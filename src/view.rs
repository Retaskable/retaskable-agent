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
    /// **Skip** on a stopped item: it's let go and skipped in the store, its branch kept.
    Skip { project: String, item: String },
    /// **Play** in lane `lane` (> 0), decided by the loop, which owns the lanes.
    Play { project: String, lane: u32 },
    /// **Stop** the game in lane `lane`.
    Stop { project: String, lane: u32 },
    /// **Generate ideas** (`ideas_generate { fresh }`): one non-quiet ideas round; `fresh` drops the waiting ideas first.
    Ideas { project: String, fresh: bool },
}

impl ItemCommand {
    fn is_lane(&self) -> bool {
        matches!(self, Self::Play { .. } | Self::Stop { .. })
    }

    fn project(&self) -> &str {
        match self {
            Self::Continue { project, .. }
            | Self::StartOver { project, .. }
            | Self::Skip { project, .. }
            | Self::Play { project, .. }
            | Self::Stop { project, .. }
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
        }
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
mod tests {
    use super::*;

    fn project(id: &str, repo: &str) -> ProjectView {
        ProjectView {
            id: id.into(),
            github_repo: repo.into(),
            backlog_path: "backlog".into(),
            repo_path: PathBuf::new(),
            worktrees_dir: PathBuf::new(),
            base_branch: "main".into(),
            galleries: vec![],
            play_command: String::new(),
        }
    }

    fn view(projects: Vec<ProjectView>) -> AgentView {
        AgentView {
            machine: "pc".into(),
            tags: vec!["windows".into()],
            projects,
            version: "1.2.3".into(),
            lanes: 3,
            chore_lane: Some(3),
            lane_sizes: LaneSizes::default(),
            paused: PauseState::default(),
            item_commands: Default::default(),
            work: Default::default(),
            restart: Default::default(),
            settings: Default::default(),
            local: Default::default(),
            live: Default::default(),
        }
    }

    #[test]
    fn snapshot_reports_the_paced_number_and_reason_only_while_set() {
        let dir = tempfile::tempdir().unwrap();
        let v = view(vec![]);
        let usage = || v.snapshot("", dir.path())["state"]["usage"].clone();
        assert_eq!(usage(), json!({}));
        v.item_commands.set_auto(Some((2, "On pace".into())));
        assert_eq!(usage(), json!({"autoConcurrency": {"allowed": 2, "reason": "On pace"}}));
        v.item_commands.set_auto(None);
        assert_eq!(usage(), json!({}));
    }

    #[test]
    fn snapshot_says_local_down_only_while_the_model_is_on_and_down_and_the_flag_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let local_down = |v: &AgentView| v.snapshot("", dir.path())["state"]["usage"].get("localDown").cloned();
        let on = |up: bool, ladder: bool| {
            let local = crate::local_model::LocalModel::new(crate::config::LocalModelConfig {
                enabled: true,
                base_url: "http://localhost:1".into(),
                model: "m".into(),
            });
            crate::tiers::set_local_ladder_for_test(ladder);
            if up {
                local.mark_up_for_test();
            } else {
                local.mark_down_for_test();
            }
            let mut v = view(vec![]);
            v.local = local;
            v
        };
        assert_eq!(local_down(&on(false, true)), Some(json!(true)));
        assert_eq!(local_down(&on(true, true)), None, "up");
        assert_eq!(local_down(&on(false, false)), None, "flag Off");
        assert_eq!(local_down(&view(vec![])), None, "model off");
    }

    #[test]
    fn pause_state_sets_replaces_and_clears_a_projects_reason() {
        let p = PauseState::default();
        assert_eq!(p.reason("a"), None);
        p.set("a", Some("lunch".into()));
        assert_eq!(p.reason("a").as_deref(), Some("lunch"));
        assert_eq!(p.reason("b"), None);
        p.set("a", Some("meeting".into()));
        assert_eq!(p.reason("a").as_deref(), Some("meeting"));
        p.set("a", None);
        assert_eq!(p.reason("a"), None);
    }

    #[test]
    fn pause_state_notifies_subscribers_only_of_real_changes() {
        let p = PauseState::default();
        let mut rx = p.subscribe();
        p.set("a", Some("x".into()));
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        p.set("a", Some("x".into()));
        assert!(!rx.has_changed().unwrap(), "same reason is no change");
        p.set("b", None);
        assert!(!rx.has_changed().unwrap(), "resuming an unpaused project is no change");
        p.set("a", None);
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn pause_states_compare_by_content() {
        let (a, b) = (PauseState::default(), PauseState::default());
        assert_eq!(a, b);
        a.set("p", Some("r".into()));
        assert_ne!(a, b);
        b.set("p", Some("r".into()));
        assert_eq!(a, b);
    }

    #[test]
    fn keys_are_the_project_ids_or_the_agents_own_before_setup() {
        assert_eq!(view(vec![]).keys(), vec![String::new()]);
        assert_eq!(view(vec![project("a", "o/a"), project("b", "o/b")]).keys(), ["a", "b"]);
    }

    #[tokio::test]
    async fn hello_carries_the_computers_settings_and_names_each_env_var_without_its_value() {
        let dir = tempfile::tempdir().unwrap();
        let env = crate::env::Env::from_pairs([("RETASKABLE_POLL_SEC", "30")]);
        let mut v = view(vec![]);
        v.settings = HelloSettings::new(
            dir.path().to_path_buf(),
            env,
            4700,
            Arc::new(crate::flags::Flags::fixed(&["machine-settings"])),
        );
        let hello = v.hello("").await;
        let s = &hello["settings"];
        assert_eq!(s["concurrency"], 1);
        assert_eq!(s["autoConcurrency"]["enabled"], true);
        assert_eq!(s["models"]["opusAbovePct"], 50);
        assert_eq!(s["port"], 4700);
        assert_eq!(s["lanes"], 2);
        assert_eq!(s["pollIntervalSec"], 30);
        assert_eq!(s["local"]["enabled"], false);
        assert_eq!(s["networkAccess"], false);
        assert!(s["engines"]["roles"].is_object() && s["cursorModels"].is_object() && s["tags"].is_array());
        assert_eq!(s["envVars"]["RETASKABLE_POLL_SEC"], true);
        assert_eq!(s["envVars"]["RETASKABLE_LANES"], false);
        assert_eq!(hello["capabilities"], json!(["agent_settings"]));
        // Off, the hello has no settings at all.
        v.settings = HelloSettings::new(
            dir.path().to_path_buf(),
            crate::env::Env::empty(),
            4700,
            Arc::new(crate::flags::Flags::fixed(&[])),
        );
        let off = v.hello("").await;
        assert!(off.get("settings").is_none());
        assert_eq!(off["capabilities"], json!([]));
        // The name is there, but the value 30 shows only as the computed pollIntervalSec, never as the variable's.
        assert!(!s["envVars"].to_string().contains("30"));
    }

    #[tokio::test]
    async fn hello_names_the_project_or_null_before_setup_or_without_a_repo() {
        let v = view(vec![project("a", "o/a"), project("b", "")]);
        let hello = v.hello("a").await;
        assert_eq!(hello["type"], "hello");
        assert_eq!(hello["agentVersion"], "1.2.3");
        assert_eq!(hello["machine"], "pc");
        assert_eq!(hello["os"], platform());
        assert_eq!(hello["tags"], json!(["windows"]));
        assert_eq!(
            hello["project"],
            json!({ "githubRepo": "o/a", "backlogPath": "backlog" })
        );
        assert!(
            v.hello("b").await["project"].is_null(),
            "a project with no repo is not Setup-done"
        );
        assert!(v.hello("").await["project"].is_null());
        assert!(v.hello("unknown").await["project"].is_null());
    }

    #[test]
    fn snapshot_before_setup_says_setup() {
        let snap = view(vec![]).snapshot("", Path::new("."));
        assert_eq!(
            snap["state"]["orchestrator"],
            json!({ "status": "setup", "machine": "pc" })
        );
        assert_eq!(snap["state"]["galleries"], json!([]));
    }

    #[test]
    fn snapshot_reports_the_trial_plan_from_the_projects_state_file() {
        use crate::trial_plan::TrialRecord;
        let dir = tempfile::tempdir().unwrap();
        let v = view(vec![project("a", "o/a")]);
        let trial = |v: &AgentView| v.snapshot("a", dir.path())["state"]["trial"].clone();
        assert_eq!(trial(&v), json!({ "state": "not_started" }));
        let mut store = Store::load(&paths::project_dir(dir.path(), "a")).unwrap();
        store.set_trial(TrialRecord::started()).unwrap();
        assert_eq!(trial(&v), json!({ "state": "running" }));
        store.set_trial(TrialRecord::opened("https://x/pr/1")).unwrap();
        assert_eq!(trial(&v), json!({ "state": "open", "prUrl": "https://x/pr/1" }));
        store.set_trial(TrialRecord::dropped()).unwrap();
        assert_eq!(trial(&v), json!({ "state": "failed" }));
        store.set_trial(TrialRecord::skip()).unwrap();
        assert_eq!(trial(&v), json!({ "state": "not_started" }));
        assert!(v.snapshot("", dir.path())["state"].get("trial").is_none());
    }

    #[test]
    fn snapshot_reports_the_pause_reason_or_the_default_one() {
        let v = view(vec![project("a", "o/a")]);
        let orch = |v: &AgentView| v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
        assert_eq!(orch(&v)["status"], "paused");
        assert_eq!(orch(&v)["pausedReason"], NO_WORK_YET);
        v.paused.set("a", Some("Paused from the site".into()));
        assert_eq!(orch(&v)["pausedReason"], "Paused from the site");
    }

    fn reporting_view(on: bool) -> AgentView {
        reporting_view_in(on, PathBuf::new())
    }

    fn reporting_view_in(on: bool, dir: PathBuf) -> AgentView {
        let mut v = view(vec![project("a", "o/a")]);
        v.settings = HelloSettings::new(
            dir,
            crate::env::Env::empty(),
            4700,
            Arc::new(crate::flags::Flags::fixed(if on {
                &[SNAPSHOT_REPORTING_FLAG]
            } else {
                &[]
            })),
        );
        v
    }

    fn lanes_of(v: &AgentView) -> Value {
        v.snapshot("a", Path::new("."))["state"]["lanes"].clone()
    }

    #[test]
    fn snapshot_reporting_shows_each_lanes_item_branch_and_game() {
        use crate::orchestrator::live::HeldLane;
        let held = |lane, playing| HeldLane {
            lane,
            item_id: format!("i{lane}"),
            title: None,
            branch: Some(format!("retaskable/i{lane}-s1")),
            playing,
        };
        let v = reporting_view(true);
        v.live.register("a");
        v.live.ticked(
            "a",
            false,
            "t".into(),
            vec![held(1, false), held(2, true), held(5, false)],
            "d".into(),
        );
        let lanes = lanes_of(&v);
        let lanes = lanes.as_array().unwrap();
        // The view has 3 lanes; lane 5 is held past that and still listed.
        assert_eq!(
            lanes.iter().map(|l| l["k"].as_u64().unwrap()).collect::<Vec<_>>(),
            [1, 2, 3, 5]
        );
        assert_eq!(
            (&lanes[0]["itemId"], &lanes[0]["branch"], &lanes[0]["playing"]),
            (&json!("i1"), &json!("retaskable/i1-s1"), &json!(false))
        );
        assert_eq!(lanes[1]["playing"], true);
        assert!(lanes[2].get("itemId").is_none() && lanes[2]["playing"] == false);
        assert_eq!(lanes[3]["itemId"], "i5");
        // Flag Off: as before.
        let off = reporting_view(false);
        off.live.register("a");
        off.live.ticked("a", false, "t".into(), vec![held(1, true)], "d".into());
        assert_eq!(lanes_of(&off)[0], json!({ "k": 1, "diskGb": 0.0, "playing": false }));
    }

    #[test]
    fn main_playing_is_true_while_play_main_runs() {
        let root = tempfile::tempdir().unwrap();
        let mut p = project("a", "o/a");
        p.repo_path = crate::test_support::repo_with_origin(root.path());
        p.worktrees_dir = root.path().join("worktrees");
        p.play_command = if cfg!(windows) {
            "ping -n 60 127.0.0.1 >nul"
        } else {
            "sleep 60"
        }
        .into();
        let v = reporting_view(true);
        let main_playing = |v: &AgentView| v.snapshot("a", Path::new("."))["state"]["mainPlaying"].clone();
        assert_eq!(main_playing(&v), false);
        crate::play::start("a", &p).unwrap();
        assert_eq!(main_playing(&v), true);
        assert_eq!(main_playing(&reporting_view(false)), false);
        crate::play::stop("a").unwrap();
        assert_eq!(main_playing(&v), false);
    }

    #[test]
    fn snapshot_reporting_gives_the_orchestrator_status_from_the_loop() {
        let v = reporting_view(true);
        let orch = |v: &AgentView| v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
        assert_eq!(orch(&v)["pausedReason"], NO_WORK_YET, "no loop running");
        assert_eq!(orch(&v)["status"], "paused");
        v.live.register("a");
        v.live.set_concurrency(3);
        assert_eq!(
            orch(&v),
            json!({ "status": "running", "machine": "pc", "concurrency": 3, "spendTodayUsd": 0.0, "dailyUsd": 0.0 })
        );
        v.live.set_limit(
            "a",
            Some(std::time::Instant::now() + std::time::Duration::from_secs(60)),
        );
        let o = orch(&v);
        assert_eq!(
            (&o["status"], o["resetsAt"].is_string()),
            (&json!("waiting_for_limit"), true)
        );
        v.paused.set("a", Some("Paused from the site".into()));
        let o = orch(&v);
        assert_eq!(
            (&o["status"], &o["pausedReason"]),
            (&json!("paused"), &json!("Paused from the site"))
        );
        v.paused.set("a", None);
        v.live.set_limit("a", None);
        v.item_commands.set_auto(Some((2, "On pace".into())));
        assert_eq!(
            orch(&v)["concurrency"],
            2,
            "auto concurrency's number once it has stepped"
        );
        assert_eq!(
            v.snapshot("", Path::new("."))["state"]["orchestrator"]["status"],
            "setup"
        );
    }

    #[test]
    fn snapshot_reporting_reports_todays_spend_and_the_daily_budget() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{ "version": 2, "budget": { "dailyUsd": 20 } }"#,
        )
        .unwrap();
        let mut machine = crate::store::MachineStore::load(dir.path()).unwrap();
        machine.add_spend("a", 1.25).unwrap();
        for on in [true, false] {
            let v = reporting_view_in(on, dir.path().to_path_buf());
            let o = v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
            assert_eq!(o.get("spendTodayUsd").is_some(), on);
            assert_eq!(o.get("dailyUsd").is_some(), on);
            if on {
                assert_eq!((&o["spendTodayUsd"], &o["dailyUsd"]), (&json!(1.25), &json!(20.0)));
            }
        }
    }

    #[test]
    fn snapshot_reporting_adds_the_usage_windows_from_the_projects_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::load(&paths::project_dir(dir.path(), "a")).unwrap();
        let soon = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 3600;
        store.set_usage(json!({"utilization": 0.25, "status": "allowed", "resetsAt": soon}));
        store.set_week_usage(json!({"rateLimitType": "seven_day", "utilization": 0.4, "resetsAt": soon}));
        store.save().unwrap();
        let usage = |on| reporting_view(on).snapshot("a", dir.path())["state"]["usage"].clone();
        let u = usage(true);
        assert_eq!(
            (&u["leftPct"], &u["status"], &u["weekUsedPct"]),
            (&json!(75.0), &json!("allowed"), &json!(40.0))
        );
        assert_eq!(u["tier"], "opus");
        assert!(u["resetsAt"].is_string() && u["weekResetsAt"].is_string());
        assert_eq!(usage(false), json!({}), "flag Off");
    }

    #[test]
    fn snapshot_reporting_off_leaves_the_orchestrator_block_as_it_was() {
        let v = reporting_view(false);
        v.live.register("a");
        v.live.set_concurrency(3);
        let orch = v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
        assert_eq!(
            orch,
            json!({ "status": "paused", "machine": "pc", "pausedReason": NO_WORK_YET })
        );
    }

    fn open(id: &str, since_ms: i64, text: &str, details: Value) -> crate::telemetry::debounce::OpenAnomaly {
        crate::telemetry::debounce::OpenAnomaly {
            id: id.into(),
            rule: "lease_idle",
            item: Some("R-1".into()),
            text: text.into(),
            details,
            since_ms,
        }
    }

    #[test]
    fn snapshot_sends_open_anomalies_oldest_first_with_their_project_and_drops_ended_ones() {
        let v = reporting_view(true);
        let orch = |v: &AgentView| v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
        v.live.register("a");
        assert!(orch(&v).get("anomalies").is_none(), "none open: the key is left out");
        let mut rx = v.live.subscribe();
        rx.mark_unchanged();
        v.live
            .set_anomalies("a", vec![open("x", 5_000, "stuck", json!({ "n": 1 }))]);
        assert!(rx.has_changed().unwrap(), "a change in the list signals the connector");
        assert_eq!(
            orch(&v)["anomalies"],
            json!([{ "id": "x", "kind": "lease_idle", "item": "R-1", "since": "1970-01-01T00:00:05.000Z",
                     "text": "stuck", "details": { "n": 1 }, "project": "a" }])
        );
        v.live.set_anomalies("a", vec![]);
        assert!(orch(&v).get("anomalies").is_none(), "gone the minute it ends");
    }

    #[test]
    fn snapshot_caps_anomalies_at_twenty_and_cleans_their_text_and_details() {
        let v = reporting_view(true);
        v.live.register("a");
        let many = (0..25).map(|i| open(&format!("id{i:02}"), i, "t", json!({}))).collect();
        v.live.set_anomalies("a", many);
        let o = v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
        assert_eq!(o["anomalies"].as_array().unwrap().len(), 20);
        assert_eq!(o["anomalies"][0]["id"], "id00");
        assert_eq!(o["anomaliesMore"], 5);

        let big: serde_json::Map<String, Value> =
            (0..40).map(|i| (format!("k{i:02}"), json!("x".repeat(100)))).collect();
        v.live.set_anomalies(
            "a",
            vec![open(
                "s",
                0,
                &format!("password=hunter2 {}", "y".repeat(400)),
                Value::Object(big),
            )],
        );
        let o = v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
        let text = o["anomalies"][0]["text"].as_str().unwrap();
        assert!(!text.contains("hunter2") && text.chars().count() <= 300, "{text}");
        let details = &o["anomalies"][0]["details"];
        assert!(details.to_string().len() <= 1024 && details["k00"].is_string());
        assert!(o.get("anomaliesMore").is_none());
    }

    #[test]
    fn snapshot_reporting_off_sends_no_anomalies() {
        let v = reporting_view(false);
        v.live.register("a");
        v.live.set_anomalies("a", vec![open("x", 0, "stuck", json!({}))]);
        assert!(
            v.snapshot("a", Path::new("."))["state"]["orchestrator"]
                .get("anomalies")
                .is_none()
        );
    }

    #[test]
    fn snapshot_reports_cursor_tokens_only_when_there_are_some_and_the_flag_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let mk = |on: bool| reporting_view_in(on, dir.path().to_path_buf());
        let cursor = |v: &AgentView| v.snapshot("", dir.path())["state"]["usage"].get("cursor").cloned();
        assert_eq!(cursor(&mk(true)), None, "no tokens yet");
        let mut m = crate::store::MachineStore::load(dir.path()).unwrap();
        m.add_cursor_tokens(30, 4).unwrap();
        m.add_cursor_tokens(1, 2).unwrap();
        m.stamp_cursor_limit_at().unwrap();
        let got = cursor(&mk(true)).unwrap();
        assert_eq!(
            (&got["todayTokens"], &got["weekInputTokens"], &got["weekOutputTokens"]),
            (&json!(37), &json!(31), &json!(6))
        );
        assert!(got["limitHitAt"].is_string());
        assert_eq!(cursor(&mk(false)), None, "flag Off");
    }

    #[test]
    fn snapshot_lists_every_lane_with_its_size_and_notes_only_the_chore_lane() {
        let v = view(vec![project("a", "o/a")]);
        v.lane_sizes.set("a", [(2, 1.5)].into_iter().collect());
        let lanes = v.snapshot("a", Path::new("."))["state"]["lanes"].clone();
        assert_eq!(
            lanes,
            json!([
                { "k": 1, "diskGb": 0.0, "playing": false },
                { "k": 2, "diskGb": 1.5, "playing": false },
                { "k": 3, "diskGb": 0.0, "playing": false, "note": CHORE_LANE_NOTE },
            ])
        );
    }

    #[test]
    fn project_view_derives_its_id_from_the_config() {
        let cfg = ProjectConfig {
            id: Some("custom".into()),
            ..ProjectConfig::default()
        };
        assert_eq!(ProjectView::from(&cfg).id, "custom");
    }
}
