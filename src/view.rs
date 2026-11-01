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

    fn project(id: &str, github_repo: &str) -> ProjectView {
        ProjectView {
            id: id.into(),
            github_repo: github_repo.into(),
            backlog_path: "backlog".into(),
            repo_path: PathBuf::new(),
            worktrees_dir: PathBuf::new(),
            base_branch: String::new(),
            galleries: vec![],
        }
    }

    fn view(projects: Vec<ProjectView>) -> AgentView {
        AgentView {
            machine: "pc".into(),
            tags: vec![],
            projects,
            version: "9.9.9".into(),
            lanes: 1,
            chore_lane: None,
            lane_sizes: LaneSizes::default(),
            paused: PauseState::default(),
        }
    }

    #[test]
    fn keys_is_the_agents_own_hub_before_any_project() {
        assert_eq!(view(vec![]).keys(), [""]);
    }

    #[test]
    fn keys_is_each_projects_id_once_there_are_any() {
        assert_eq!(
            view(vec![project("one", "o/one"), project("two", "o/two")]).keys(),
            ["one", "two"]
        );
    }

    #[test]
    fn hello_has_no_project_before_setup() {
        let v = view(vec![]);
        assert_eq!(v.hello("")["project"], Value::Null);
    }

    #[test]
    fn hello_has_no_project_while_its_repo_is_still_unset() {
        // A project can exist (and so appear in `keys`) before Setup has given it a repo.
        let v = view(vec![project("one", "")]);
        assert_eq!(v.hello("one")["project"], Value::Null);
    }

    #[test]
    fn hello_names_the_matching_projects_repo_and_backlog() {
        let v = view(vec![project("one", "o/one"), project("two", "o/two")]);
        assert_eq!(
            v.hello("two")["project"],
            json!({ "githubRepo": "o/two", "backlogPath": "backlog" })
        );
    }

    #[test]
    fn hello_truncates_the_version_and_machine() {
        let mut v = view(vec![]);
        v.version = "v".repeat(60);
        v.machine = "m".repeat(250);
        let hello = v.hello("");
        assert_eq!(hello["agentVersion"].as_str().unwrap().len(), 50);
        assert_eq!(hello["machine"].as_str().unwrap().len(), 200);
    }

    #[test]
    fn pause_state_starts_with_no_reason() {
        assert_eq!(PauseState::default().reason("a"), None);
    }

    #[test]
    fn pause_state_set_and_clear_round_trip() {
        let paused = PauseState::default();
        paused.set("a", Some("why".into()));
        assert_eq!(paused.reason("a"), Some("why".into()));
        paused.set("a", None);
        assert_eq!(paused.reason("a"), None);
    }

    #[test]
    fn pause_state_only_signals_a_change_when_the_reason_actually_changes() {
        let paused = PauseState::default();
        let mut rx = paused.subscribe();
        assert!(!rx.has_changed().unwrap());

        paused.set("a", Some("why".into()));
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();

        // Setting the same reason again is a no-op: nothing watching should resend.
        paused.set("a", Some("why".into()));
        assert!(!rx.has_changed().unwrap());

        // Clearing a project that was never paused is also a no-op.
        paused.set("b", None);
        assert!(!rx.has_changed().unwrap());

        paused.set("a", None);
        assert!(rx.has_changed().unwrap());
    }
}
