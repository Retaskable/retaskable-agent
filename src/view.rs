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
use crate::lane_pool::LaneSizes;
use crate::link::truncate;

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
            | Self::Stop { project, .. } => project,
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

/// A question from the app's chat (`ask`), queued for the loop's ask step (agent/06.6-ask-claude.md). `images` is the
/// "Attached images…" block made from the message's saved pictures, empty when it has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskCommand {
    pub project: String,
    pub ask_id: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub about: Option<String>,
    pub images: String,
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

    pub fn push_ask(&self, ask: AskCommand) {
        self.asks.lock().unwrap_or_else(|e| e.into_inner()).push(ask);
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
    pub fn hello(&self, project: &str) -> Value {
        let repo = self
            .projects
            .iter()
            .find(|p| p.id == project && !p.github_repo.is_empty());
        json!({
            "type": "hello",
            "agentVersion": truncate(&self.version, 50),
            "os": platform(),
            "machine": truncate(&self.machine, 200),
            "capabilities": [],
            "policy": {},
            "project": repo.map(|p| json!({ "githubRepo": p.github_repo, "backlogPath": p.backlog_path })),
            "tags": self.tags,
        })
    }

    /// The project's snapshot (cloud.md section 5.2): every list the hub needs. Most are still
    /// empty, since this agent runs no work yet, but a project's galleries report their real
    /// status and last shoot (agent/09-web-ui.md section 9.2), and an orchestrator that says so.
    pub fn snapshot(&self, project: &str, data_dir: &Path) -> Value {
        let orchestrator = if project.is_empty() {
            json!({ "status": "setup", "machine": self.machine })
        } else {
            let reason = self.paused.reason(project).unwrap_or_else(|| NO_WORK_YET.to_string());
            json!({ "status": "paused", "machine": self.machine, "pausedReason": reason })
        };
        let mut orchestrator = orchestrator;
        if let Some(restart) = self.restart.report() {
            orchestrator["restart"] = restart;
        }
        json!({
            "type": "snapshot",
            "state": {
                "orchestrator": orchestrator,
                "usage": {},
                "milestones": [],
                "items": [],
                "sessions": [],
                "lanes": self.lanes_list(project),
                "mainPlaying": false,
                "galleries": self.galleries_list(project, data_dir),
                "drafts": [],
                "asks": [],
            },
        })
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
        (1..=self.lanes)
            .map(|k| {
                let mut lane = json!({ "k": k, "diskGb": self.lane_sizes.gb(project, k), "playing": false });
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
        }
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

    #[test]
    fn hello_names_the_project_or_null_before_setup_or_without_a_repo() {
        let v = view(vec![project("a", "o/a"), project("b", "")]);
        let hello = v.hello("a");
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
            v.hello("b")["project"].is_null(),
            "a project with no repo is not Setup-done"
        );
        assert!(v.hello("")["project"].is_null());
        assert!(v.hello("unknown")["project"].is_null());
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
    fn snapshot_reports_the_pause_reason_or_the_default_one() {
        let v = view(vec![project("a", "o/a")]);
        let orch = |v: &AgentView| v.snapshot("a", Path::new("."))["state"]["orchestrator"].clone();
        assert_eq!(orch(&v)["status"], "paused");
        assert_eq!(orch(&v)["pausedReason"], NO_WORK_YET);
        v.paused.set("a", Some("Paused from the site".into()));
        assert_eq!(orch(&v)["pausedReason"], "Paused from the site");
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
