//! Each project's own settings (version 2's `projects[]`, or version 1's whole file, read as the one project),
//! and the types that make it up.
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// `projectType` (default "game"): picks the UI's words (project-types.md).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectType {
    #[default]
    Game,
    Web,
    Server,
    Cli,
    Other,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxMode {
    #[default]
    Auto,
    Off,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnOff {
    #[default]
    On,
    Off,
}

/// `syncMain`: `auto` (the default) keeps the base branch caught up with main through a `sync_main` chore; `off`
/// never starts one (team-branch.md section 4).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncMain {
    #[default]
    Auto,
    Off,
}

/// `plans`: `risky` (the default) lets a plan someone asked for go ahead on that yes unless the drafter marked it
/// risky; `ask` waits for Approve on every plan (agent/06.5-spec-drafts.md).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Plans {
    #[default]
    Risky,
    Ask,
}

/// on: wait for the playtest before merging. after_merge: merge, then ask. off: never ask.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Playtests {
    On,
    #[default]
    AfterMerge,
    Off,
}

#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckConfig {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default)]
    pub timeout_sec: u64,
}

/// A tab of pictures the game takes of itself (spec section 6.8).
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GalleryConfig {
    #[serde(default)]
    pub name: String,
    /// Run in the base checkout; writes pictures to out_dir, then exits. Empty: the game can't yet.
    #[serde(default)]
    pub command: String,
    /// Relative to the repo root.
    #[serde(default)]
    pub out_dir: String,
}

impl GalleryConfig {
    /// This gallery's slug (agent/09-web-ui.md section 9.1): its name in kebab case, the same way
    /// a project's id is made from its folder name. Names its tab, its folder and the `gallery`
    /// the hub's Shoot command names it by.
    pub fn slug(&self) -> String {
        kebab_case(&self.name.to_lowercase())
    }
}

fn default_galleries() -> Vec<GalleryConfig> {
    vec![
        GalleryConfig {
            name: "Models".to_string(),
            command: String::new(),
            out_dir: "target/models".to_string(),
        },
        GalleryConfig {
            name: "UI".to_string(),
            command: String::new(),
            out_dir: "target/ui".to_string(),
        },
    ]
}

/// Scouts split the correctness scout above `max_scout_diff_lines`, and don't run below `min_scout_diff_lines`.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewConfig {
    #[serde(default = "default_max_scout_diff_lines")]
    pub max_scout_diff_lines: u32,
    #[serde(default = "default_min_scout_diff_lines")]
    pub min_scout_diff_lines: u32,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        Self {
            max_scout_diff_lines: default_max_scout_diff_lines(),
            min_scout_diff_lines: default_min_scout_diff_lines(),
        }
    }
}

fn default_max_scout_diff_lines() -> u32 {
    4000
}
fn default_min_scout_diff_lines() -> u32 {
    2000
}

/// Tuning knobs for the orchestrator loop itself (07-orchestrator-loop.md's watchdog). 0 turns the
/// quiet watchdog off.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TuningConfig {
    #[serde(default = "default_session_quiet_minutes")]
    pub session_quiet_minutes: u32,
    /// `watchdog.lease_idle`'s threshold (docs/specs/agent-telemetry.md section 4).
    #[serde(default = "default_lease_idle_minutes")]
    pub lease_idle_minutes: u32,
    /// `watchdog.idle_lanes`'s threshold.
    #[serde(default = "default_idle_lanes_minutes")]
    pub idle_lanes_minutes: u32,
    /// `watchdog.no_progress`'s threshold.
    #[serde(default = "default_no_progress_minutes")]
    pub no_progress_minutes: u32,
    /// How long a watch waits for any check to show up before reading "no CI" as a pass
    /// (`ciGraceMinutes`, 05-item-lifecycle.md's `awaiting_ci` row).
    #[serde(default = "default_ci_grace_minutes")]
    pub ci_grace_minutes: u32,
    /// The most a watch waits for pending checks (`ciMaxMinutes`).
    #[serde(default = "default_ci_max_minutes")]
    pub ci_max_minutes: u32,
}

impl Default for TuningConfig {
    fn default() -> Self {
        Self {
            session_quiet_minutes: default_session_quiet_minutes(),
            lease_idle_minutes: default_lease_idle_minutes(),
            idle_lanes_minutes: default_idle_lanes_minutes(),
            no_progress_minutes: default_no_progress_minutes(),
            ci_grace_minutes: default_ci_grace_minutes(),
            ci_max_minutes: default_ci_max_minutes(),
        }
    }
}

fn default_session_quiet_minutes() -> u32 {
    20
}

fn default_ci_grace_minutes() -> u32 {
    10
}

fn default_ci_max_minutes() -> u32 {
    120
}

fn default_lease_idle_minutes() -> u32 {
    crate::telemetry::watchdog::LEASE_IDLE_MINUTES
}

fn default_idle_lanes_minutes() -> u32 {
    crate::telemetry::watchdog::IDLE_LANES_MINUTES
}

fn default_no_progress_minutes() -> u32 {
    crate::telemetry::watchdog::NO_PROGRESS_MINUTES
}

/// Checked before every push (agent.md section 6.2). 0 turns one off.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeGates {
    #[serde(default = "default_max_file_lines")]
    pub max_file_lines: u32,
    #[serde(default = "default_dup_lines")]
    pub dup_lines: u32,
    #[serde(default = "default_max_spec_chars")]
    pub max_spec_chars: u32,
}

impl Default for CodeGates {
    fn default() -> Self {
        Self {
            max_file_lines: default_max_file_lines(),
            dup_lines: default_dup_lines(),
            max_spec_chars: default_max_spec_chars(),
        }
    }
}

fn default_max_file_lines() -> u32 {
    1000
}
fn default_dup_lines() -> u32 {
    8
}
fn default_max_spec_chars() -> u32 {
    40_000
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectBudget {
    #[serde(default)]
    pub planner_usd: f64,
    #[serde(default)]
    pub implementer_usd: f64,
    #[serde(default)]
    pub scout_usd: f64,
    #[serde(default)]
    pub reviewer_usd: f64,
    #[serde(default)]
    pub drafter_usd: f64,
    #[serde(default)]
    pub ask_usd: f64,
    #[serde(default)]
    pub release_notes_usd: f64,
    /// Minutes a Cursor session may run before it's killed (engines/04-running-cursor-agent.md);
    /// 60 when not set, and 0 means the same.
    #[serde(default = "default_cursor_max_minutes")]
    pub cursor_max_minutes: u32,
}

fn default_cursor_max_minutes() -> u32 {
    60
}

impl Default for ProjectBudget {
    fn default() -> Self {
        Self {
            planner_usd: 0.0,
            implementer_usd: 0.0,
            scout_usd: 0.0,
            reviewer_usd: 0.0,
            drafter_usd: 0.0,
            ask_usd: 0.0,
            release_notes_usd: 0.0,
            cursor_max_minutes: default_cursor_max_minutes(),
        }
    }
}

/// Where the project deploys from (environments.md section 4.1). Missing: none.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployConfig {
    #[serde(default)]
    pub staging: Option<DeployTarget>,
    #[serde(default)]
    pub production: Option<DeployTarget>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployTarget {
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub smoke: Option<String>,
    #[serde(default)]
    pub version_path: Option<String>,
}

/// Play on phone (play-on-phone.md section 2): builds the Android app and installs it on a phone over adb.
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PhoneConfig {
    #[serde(default)]
    pub app_dir: String,
    #[serde(default)]
    pub build_command: String,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub item_id: Option<String>,
}

/// The daily Lighthouse chore's setting (daily-lighthouse.md section 2): off by default. The agent only reads it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LighthouseConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_lighthouse_pages")]
    pub pages: Vec<String>,
    /// The hour of the day (project time zone) the chore becomes due.
    #[serde(default = "default_lighthouse_hour")]
    pub hour: u32,
    #[serde(default = "default_lighthouse_min_score")]
    pub min_score: u32,
}

fn default_lighthouse_pages() -> Vec<String> {
    vec!["/".to_string()]
}

fn default_lighthouse_hour() -> u32 {
    6
}

fn default_lighthouse_min_score() -> u32 {
    90
}

/// The daily vetting chore's setting (vet-suggestions.md): off by default. The agent only reads it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VetSuggestionsConfig {
    #[serde(default)]
    pub enabled: bool,
}

impl Default for LighthouseConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            pages: default_lighthouse_pages(),
            hour: default_lighthouse_hour(),
            min_score: default_lighthouse_min_score(),
        }
    }
}

/// One project's settings (version 2's `projects[]`, or version 1's whole file, read as the one project).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfig {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub repo_path: String,
    #[serde(default)]
    pub project_type: ProjectType,
    #[serde(default)]
    pub github_repo: String,
    #[serde(default)]
    pub backlog_path: String,
    #[serde(default)]
    pub base_branch: String,
    /// The GitHub workflow pattern and branch rules (github-workflow.md, not yet ported here): kept as written.
    #[serde(default)]
    pub github: Option<Value>,
    #[serde(default)]
    pub require_ci: bool,
    #[serde(default)]
    pub ci_covers_checks: bool,
    #[serde(default)]
    pub ci_when_ready: bool,
    #[serde(default)]
    pub sandbox: SandboxMode,
    #[serde(default)]
    pub worktrees_dir: String,
    #[serde(default)]
    pub checks: Vec<CheckConfig>,
    #[serde(default)]
    pub play_command: String,
    #[serde(default = "default_galleries")]
    pub galleries: Vec<GalleryConfig>,
    #[serde(default)]
    pub playtests: Playtests,
    #[serde(default)]
    pub plans: Plans,
    #[serde(default)]
    pub chores: OnOff,
    #[serde(default)]
    pub sync_main: SyncMain,
    #[serde(default)]
    pub triage: OnOff,
    #[serde(default = "default_max_review_rounds")]
    pub max_review_rounds: u32,
    #[serde(default = "default_max_conflict_rounds")]
    pub max_conflict_rounds: u32,
    #[serde(default = "default_max_check_rounds")]
    pub max_check_rounds: u32,
    #[serde(default = "default_max_slices_per_item")]
    pub max_slices_per_item: u32,
    #[serde(default)]
    pub review: ReviewConfig,
    #[serde(default)]
    pub code_gates: CodeGates,
    #[serde(default)]
    pub budget: ProjectBudget,
    #[serde(default)]
    pub deploy: DeployConfig,
    #[serde(default)]
    pub phone: Option<PhoneConfig>,
    /// Claude Code vs. Cursor per role (engines.md, not yet ported here): kept as written.
    #[serde(default)]
    pub engines: Option<Value>,
    #[serde(default)]
    pub tuning: TuningConfig,
    #[serde(default)]
    pub lighthouse: LighthouseConfig,
    #[serde(default)]
    pub vet_suggestions: VetSuggestionsConfig,
}

impl ProjectConfig {
    /// This project's id: the one it was given, or, for an old file that predates the field
    /// (version 1), the one derived from its `repoPath` the same way `load` fills it in.
    pub fn id_or_derived(&self) -> String {
        self.id.clone().unwrap_or_else(|| project_id(&self.repo_path))
    }
}

fn default_max_review_rounds() -> u32 {
    3
}
fn default_max_conflict_rounds() -> u32 {
    5
}
fn default_max_check_rounds() -> u32 {
    3
}
fn default_max_slices_per_item() -> u32 {
    10
}

/// A project's id, made once from its repo folder's name in kebab case (`retaskable.com` gives `retaskable-com`).
/// Resolved against the current directory first, as `path.resolve` does in the TypeScript agent's `projectId`,
/// so a relative `repoPath` (`.`, `..`) gives the folder's own name rather than the relative segment's.
pub(super) fn project_id(repo_path: &str) -> String {
    let resolved = std::path::absolute(repo_path).unwrap_or_else(|_| PathBuf::from(repo_path));
    let normalized = resolve_lexically(&resolved);
    let name = normalized
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    kebab_case(&name)
}

/// `std::path::absolute` resolves `..` on Windows but, per its own docs, "keeps `..` components"
/// on POSIX. A `repoPath` of `..` would then leave the resolved path ending in `..`, whose
/// `file_name()` is `None` on every platform — silently falling back to the id "project" on
/// Linux/macOS instead of naming the parent directory the way Windows (and the TypeScript
/// agent's `path.resolve`) does. Collapse `.` and `..` components ourselves so both platforms agree.
fn resolve_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn kebab_case(name: &str) -> String {
    let kebab: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = kebab.trim_matches('-');
    let collapsed: Vec<&str> = trimmed.split('-').filter(|s| !s.is_empty()).collect();
    if collapsed.is_empty() {
        "project".to_string()
    } else {
        collapsed.join("-")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_max_minutes_defaults_to_sixty_and_reads_the_budget() {
        let p: ProjectConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(p.budget.cursor_max_minutes, 60);
        let p: ProjectConfig = serde_json::from_str(r#"{"budget":{"cursorMaxMinutes":15}}"#).unwrap();
        assert_eq!(p.budget.cursor_max_minutes, 15);
    }

    #[test]
    fn lighthouse_is_off_by_default_and_reads_its_fields() {
        let p: ProjectConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(
            p.lighthouse,
            LighthouseConfig {
                enabled: false,
                pages: vec!["/".into()],
                hour: 6,
                min_score: 90
            }
        );
        let p: ProjectConfig =
            serde_json::from_str(r#"{"lighthouse":{"enabled":true,"pages":["/a","/b"],"hour":3,"minScore":80}}"#)
                .unwrap();
        assert_eq!(
            p.lighthouse,
            LighthouseConfig {
                enabled: true,
                pages: vec!["/a".into(), "/b".into()],
                hour: 3,
                min_score: 80
            }
        );
    }

    #[test]
    fn vet_suggestions_is_off_by_default_and_reads_enabled() {
        let p: ProjectConfig = serde_json::from_str("{}").unwrap();
        assert!(!p.vet_suggestions.enabled);
        let p: ProjectConfig = serde_json::from_str(r#"{"vetSuggestions":{"enabled":true}}"#).unwrap();
        assert!(p.vet_suggestions.enabled);
    }

    #[test]
    fn ci_tuning_defaults_to_ten_and_one_hundred_twenty_minutes_and_can_be_set() {
        let t = TuningConfig::default();
        assert_eq!((t.ci_grace_minutes, t.ci_max_minutes), (10, 120));
        let t: TuningConfig = serde_json::from_str(r#"{"ciGraceMinutes":3,"ciMaxMinutes":30}"#).unwrap();
        assert_eq!((t.ci_grace_minutes, t.ci_max_minutes), (3, 30));
        let t: TuningConfig = serde_json::from_str("{}").unwrap();
        assert_eq!((t.ci_grace_minutes, t.ci_max_minutes), (10, 120));
    }

    #[test]
    fn a_relative_repo_path_resolves_against_the_current_directory_first() {
        // TypeScript's projectId runs path.basename(path.resolve(repoPath)): a relative repoPath such as
        // "." or ".." gives the resolved folder's name, not the literal segment's. The expected id still
        // goes through the same kebab-casing as project_id itself, since the folder name (e.g. a worktree
        // like "_planner") isn't guaranteed to already be kebab-case.
        let current_dir_name = std::env::current_dir()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_lowercase();
        let expected = kebab_case(&current_dir_name);
        assert_eq!(project_id("."), expected);
    }

    #[test]
    fn resolve_lexically_collapses_a_trailing_parent_dir_component() {
        // std::path::absolute keeps `..` on POSIX instead of resolving it (see its own docs),
        // which is exactly the shape a repoPath of ".." produces. Without this collapsing step,
        // file_name() on a path ending in ".." returns None on every platform.
        assert_eq!(resolve_lexically(Path::new("/foo/bar/..")), Path::new("/foo"));
    }

    #[test]
    fn resolve_lexically_clamps_at_the_root_instead_of_going_above_it() {
        assert_eq!(resolve_lexically(Path::new("/foo/../..")), Path::new("/"));
    }

    #[test]
    fn project_id_of_a_parent_directory_repo_path_names_the_parent_not_project() {
        // Exercises the exact ".." example the doc comment on project_id promises, through a
        // path shape (trailing "..") that std::path::absolute leaves unresolved on POSIX.
        // A temp folder stands in for the current directory's parent, which has no name when the
        // checkout is mounted at the root of a container (`/work`).
        let parent = tempfile::tempdir().unwrap();
        let parent_name = parent.path().file_name().unwrap().to_string_lossy().to_lowercase();
        let up = parent.path().join("child").join("..");
        assert_eq!(project_id(&up.to_string_lossy()), kebab_case(&parent_name));
    }

    #[test]
    fn kebab_case_turns_dots_into_the_dash_the_doc_comment_promises() {
        // The doc comment above project_id spells out this exact example.
        assert_eq!(kebab_case("retaskable.com"), "retaskable-com");
    }

    #[test]
    fn kebab_case_leaves_an_already_kebab_name_alone() {
        assert_eq!(kebab_case("retaskable-agent"), "retaskable-agent");
    }

    #[test]
    fn kebab_case_collapses_runs_of_non_alphanumeric_characters() {
        assert_eq!(kebab_case("my_cool   game!!"), "my-cool-game");
    }

    #[test]
    fn kebab_case_trims_leading_and_trailing_separators() {
        assert_eq!(kebab_case("--retaskable--"), "retaskable");
    }

    #[test]
    fn kebab_case_of_an_empty_string_is_project() {
        assert_eq!(kebab_case(""), "project");
    }

    #[test]
    fn kebab_case_of_only_separators_is_project() {
        assert_eq!(kebab_case("..."), "project");
    }

    #[test]
    fn id_or_derived_keeps_an_explicit_id_without_deriving_one() {
        let config = ProjectConfig {
            id: Some("explicit-id".to_string()),
            ..empty_config()
        };
        assert_eq!(config.id_or_derived(), "explicit-id");
    }

    #[test]
    fn id_or_derived_falls_back_to_project_id_when_none_was_given() {
        let config = ProjectConfig {
            id: None,
            repo_path: ".".to_string(),
            ..empty_config()
        };
        assert_eq!(config.id_or_derived(), project_id("."));
    }

    fn empty_config() -> ProjectConfig {
        ProjectConfig {
            id: None,
            repo_path: String::new(),
            project_type: ProjectType::default(),
            github_repo: String::new(),
            backlog_path: String::new(),
            base_branch: String::new(),
            github: None,
            require_ci: false,
            ci_covers_checks: false,
            ci_when_ready: false,
            sandbox: SandboxMode::default(),
            worktrees_dir: String::new(),
            checks: Vec::new(),
            play_command: String::new(),
            galleries: default_galleries(),
            playtests: Playtests::default(),
            plans: Plans::default(),
            chores: OnOff::default(),
            sync_main: SyncMain::default(),
            triage: OnOff::default(),
            max_review_rounds: default_max_review_rounds(),
            max_conflict_rounds: default_max_conflict_rounds(),
            max_check_rounds: default_max_check_rounds(),
            max_slices_per_item: default_max_slices_per_item(),
            review: ReviewConfig::default(),
            code_gates: CodeGates::default(),
            budget: ProjectBudget::default(),
            deploy: DeployConfig::default(),
            phone: None,
            engines: None,
            tuning: TuningConfig::default(),
            lighthouse: LighthouseConfig::default(),
            vet_suggestions: VetSuggestionsConfig::default(),
        }
    }
}
