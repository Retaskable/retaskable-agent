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

/// on: wait for the playtest before merging. after_merge: merge, then ask. off: never ask.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Playtests {
    On,
    #[default]
    AfterMerge,
    Off,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Deserialize)]
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

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
    pub chores: OnOff,
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
        let parent_name = std::env::current_dir()
            .unwrap()
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_lowercase();
        let expected = kebab_case(&parent_name);
        assert_eq!(project_id(".."), expected);
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
            chores: OnOff::default(),
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
        }
    }
}
