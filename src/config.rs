//! `.retaskable/config.json`, read the same way as the TypeScript agent (Retaskable.com agent/src/config.ts and
//! paths.ts), following docs/specs/agent/10-config-retaskable-config-json.md: the machine's settings at the top
//! and each project's in `projects[]` (version 2), or the whole file read as one project (version 1, no
//! `"version"` key). `engines`, `cursor` and `github` are kept as raw JSON for now: their own specs (engines.md,
//! github-workflow.md) aren't ported to this agent yet. Like the TypeScript agent, nothing here validates a
//! project's fields at parse time — a value missing from a hand-edited or partial file just reads as empty.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;

pub const DEFAULT_PORT: u16 = 4700;

/// The settings file: the machine's own settings, shared by every project here, and each project's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigFile {
    pub machine: MachineConfig,
    pub projects: Vec<ProjectConfig>,
}

/// The machine's own settings (`MACHINE_KEYS` in the TypeScript agent): at the top of the file in both versions.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineConfig {
    #[serde(default = "default_lanes")]
    pub lanes: u32,
    #[serde(default = "default_concurrency")]
    pub concurrency: u32,
    #[serde(default = "default_poll_interval_sec")]
    pub poll_interval_sec: u32,
    /// Past this many tokens an implementer's follow-up starts a fresh session rather than resuming.
    #[serde(default = "default_max_resume_context_tokens")]
    pub max_resume_context_tokens: u32,
    /// Claude Code's `--max-turns` for an implementer session.
    #[serde(default = "default_max_turns")]
    pub max_turns: u32,
    #[serde(default)]
    pub models: ModelsConfig,
    #[serde(default)]
    pub local: LocalModelConfig,
    /// The Cursor model per tier, `auto` when missing (engines.md section 3.2): kept as written.
    #[serde(default)]
    pub cursor: Option<Value>,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub budget: MachineBudget,
}

impl Default for MachineConfig {
    fn default() -> Self {
        Self {
            lanes: default_lanes(),
            concurrency: default_concurrency(),
            poll_interval_sec: default_poll_interval_sec(),
            max_resume_context_tokens: default_max_resume_context_tokens(),
            max_turns: default_max_turns(),
            models: ModelsConfig::default(),
            local: LocalModelConfig::default(),
            cursor: None,
            port: default_port(),
            budget: MachineBudget::default(),
        }
    }
}

fn default_lanes() -> u32 {
    2
}
fn default_concurrency() -> u32 {
    1
}
fn default_poll_interval_sec() -> u32 {
    10
}
fn default_max_resume_context_tokens() -> u32 {
    150_000
}
fn default_max_turns() -> u32 {
    300
}
fn default_port() -> u16 {
    DEFAULT_PORT
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsConfig {
    #[serde(default = "default_opus_above_pct")]
    pub opus_above_pct: u32,
    #[serde(default = "default_sonnet_above_pct")]
    pub sonnet_above_pct: u32,
    #[serde(default = "default_pause_below_pct")]
    pub pause_below_pct: u32,
}

impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            opus_above_pct: default_opus_above_pct(),
            sonnet_above_pct: default_sonnet_above_pct(),
            pause_below_pct: default_pause_below_pct(),
        }
    }
}

fn default_opus_above_pct() -> u32 {
    50
}
fn default_sonnet_above_pct() -> u32 {
    20
}
fn default_pause_below_pct() -> u32 {
    5
}

/// A local Anthropic-compatible model server (spec section 7.2). Missing or `enabled: false` means off.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalModelConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_local_base_url")]
    pub base_url: String,
    #[serde(default)]
    pub model: String,
}

impl Default for LocalModelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_local_base_url(),
            model: String::new(),
        }
    }
}

fn default_local_base_url() -> String {
    "http://localhost:11436".to_string()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachineBudget {
    #[serde(default)]
    pub daily_usd: f64,
}

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
#[derive(Debug, Clone, PartialEq, Deserialize)]
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
fn project_id(repo_path: &str) -> String {
    let name = Path::new(repo_path)
        .file_name()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
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

/// `RETASKABLE_DATA_DIR`, or the working folder's `.retaskable/`, as a checkout of the TypeScript agent uses.
pub fn data_dir(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    env("RETASKABLE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".retaskable"))
}

/// The config file, or the defaults when there's none yet.
pub fn load(dir: &Path) -> Result<ConfigFile> {
    let file = dir.join("config.json");
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ConfigFile::default()),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    parse(&text).with_context(|| format!("reading {}", file.display()))
}

/// A version 2 file as it is; a version 1 one (no `"version"` key) as one project, its id made when it has none
/// (agent/10-config-retaskable-config-json.md).
fn parse(text: &str) -> Result<ConfigFile> {
    let raw: Value = serde_json::from_str(text)?;
    let machine: MachineConfig = serde_json::from_value(raw.clone())?;
    if raw.get("version").and_then(Value::as_u64) == Some(2) {
        let projects = match raw.get("projects") {
            Some(v) => serde_json::from_value(v.clone())?,
            None => Vec::new(),
        };
        return Ok(ConfigFile { machine, projects });
    }
    let mut project: ProjectConfig = serde_json::from_value(raw)?;
    if project.id.is_none() {
        project.id = Some(project_id(&project.repo_path));
    }
    Ok(ConfigFile {
        machine,
        projects: vec![project],
    })
}

// agent-settings.md section 3.1: "Port: a whole number from 1024 to 65535." Anything below
// that (0 included) is as invalid as an unparseable value and falls through the same way.
fn in_range(p: u16) -> bool {
    p >= 1024
}

pub fn port(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> u16 {
    env("RETASKABLE_PORT")
        .and_then(|p| p.parse().ok())
        .filter(|&p| in_range(p))
        .or_else(|| Some(cfg.machine.port).filter(|&p| in_range(p)))
        .unwrap_or(DEFAULT_PORT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn with_port(port: u16) -> ConfigFile {
        ConfigFile {
            machine: MachineConfig {
                port,
                ..MachineConfig::default()
            },
            ..ConfigFile::default()
        }
    }

    #[test]
    fn port_prefers_the_env_then_the_file_then_4700() {
        let env = |k: &str| (k == "RETASKABLE_PORT").then(|| "4800".to_string());
        assert_eq!(port(&with_port(4900), env), 4800);
        assert_eq!(port(&with_port(4900), no_env), 4900);
        assert_eq!(port(&ConfigFile::default(), no_env), DEFAULT_PORT);
        let bad = |k: &str| (k == "RETASKABLE_PORT").then(|| "nope".to_string());
        assert_eq!(port(&ConfigFile::default(), bad), DEFAULT_PORT);
        // An unparseable env value falls through to the file, not straight to 4700.
        assert_eq!(port(&with_port(4900), bad), 4900);
    }

    #[test]
    fn a_zero_env_port_is_not_used_either() {
        let zero = |k: &str| (k == "RETASKABLE_PORT").then(|| "0".to_string());
        assert_eq!(port(&with_port(4900), zero), 4900);
        assert_eq!(port(&ConfigFile::default(), zero), DEFAULT_PORT);
    }

    #[test]
    fn a_zero_port_in_the_file_is_not_used_either() {
        // Binding port 0 asks the OS for a random ephemeral port, not the documented
        // fallback, so a zero in config.json is as invalid as a zero env value.
        assert_eq!(port(&with_port(0), no_env), DEFAULT_PORT);
    }

    #[test]
    fn a_privileged_port_below_1024_is_not_used_either() {
        // agent-settings.md section 3.1: the documented valid range is 1024 to 65535,
        // so a sub-1024 port (env or file) is as invalid as zero and falls through too.
        let low_env = |k: &str| (k == "RETASKABLE_PORT").then(|| "80".to_string());
        assert_eq!(port(&with_port(4900), low_env), 4900);
        assert_eq!(port(&ConfigFile::default(), low_env), DEFAULT_PORT);
        assert_eq!(port(&with_port(80), no_env), DEFAULT_PORT);
        assert_eq!(port(&with_port(1024), no_env), 1024);
    }

    #[test]
    fn a_port_at_the_top_of_the_documented_range_is_used() {
        // agent-settings.md section 3.1: the documented valid range runs up to 65535,
        // so the top of the range is as valid as the bottom (already covered at 1024).
        let top_env = |k: &str| (k == "RETASKABLE_PORT").then(|| "65535".to_string());
        assert_eq!(port(&ConfigFile::default(), top_env), 65535);
        assert_eq!(port(&with_port(65535), no_env), 65535);
    }

    #[test]
    fn a_missing_file_is_the_defaults_and_no_projects() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.machine.port, DEFAULT_PORT);
        assert!(cfg.projects.is_empty());
    }

    #[test]
    fn a_version_2_file_reads_the_machine_settings_and_each_project() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{
                "version": 2,
                "port": 4711,
                "lanes": 4,
                "projects": [
                    { "id": "void-miner", "repoPath": "C:/code/void-miner", "githubRepo": "jamez667/void-miner",
                      "backlogPath": "docs/specs/build-order.md", "baseBranch": "main", "worktreesDir": "C:/code/.wt" }
                ]
            }"#,
        )
        .unwrap();
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.machine.port, 4711);
        assert_eq!(cfg.machine.lanes, 4);
        assert_eq!(cfg.projects.len(), 1);
        let p = &cfg.projects[0];
        assert_eq!(p.id, Some("void-miner".to_string()));
        assert_eq!(p.repo_path, "C:/code/void-miner");
        assert_eq!(p.project_type, ProjectType::Game);
        assert_eq!(p.sandbox, SandboxMode::Auto);
        assert_eq!(p.playtests, Playtests::AfterMerge);
        assert_eq!(p.max_review_rounds, 3);
        assert_eq!(p.code_gates.max_file_lines, 1000);
    }

    #[test]
    fn a_version_1_file_is_read_as_one_project_with_an_id_from_its_repo_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{
                "repoPath": "C:/Users/mail/working/Personal/Games/void-miner",
                "projectType": "game",
                "githubRepo": "jamez667/void-miner",
                "backlogPath": "docs/specs/build-order.md",
                "baseBranch": "main",
                "requireCi": false,
                "worktreesDir": "C:/Users/mail/working/Personal/Games/.retaskable-worktrees/void-miner",
                "lanes": 2,
                "concurrency": 1,
                "checks": [{ "name": "test", "command": "cargo test", "blocking": true, "timeoutSec": 1800 }],
                "review": { "maxScoutDiffLines": 2000, "minScoutDiffLines": 1000 },
                "codeGates": { "maxFileLines": 1000, "dupLines": 8, "maxSpecChars": 40000 },
                "sandbox": "auto",
                "playCommand": "cargo run --release -- --play",
                "playtests": "after_merge",
                "chores": "on",
                "triage": "on",
                "maxSlicesPerItem": 10,
                "models": { "opusAbovePct": 50, "sonnetAbovePct": 20, "pauseBelowPct": 5 },
                "budget": { "plannerUsd": 1.5, "implementerUsd": 5, "scoutUsd": 0.3, "reviewerUsd": 1.5, "drafterUsd": 2, "askUsd": 0.5, "releaseNotesUsd": 0.5, "dailyUsd": 0 },
                "port": 4700,
                "local": { "enabled": false, "baseUrl": "http://localhost:11436", "model": "" }
            }"#,
        )
        .unwrap();
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.machine.port, 4700);
        assert_eq!(cfg.machine.lanes, 2);
        assert_eq!(cfg.machine.concurrency, 1);
        assert_eq!(cfg.projects.len(), 1);
        let p = &cfg.projects[0];
        assert_eq!(p.id, Some("void-miner".to_string()));
        assert_eq!(p.repo_path, "C:/Users/mail/working/Personal/Games/void-miner");
        assert_eq!(p.github_repo, "jamez667/void-miner");
        assert_eq!(p.checks.len(), 1);
        assert_eq!(p.checks[0].command, "cargo test");
        assert_eq!(p.review.max_scout_diff_lines, 2000);
        assert_eq!(p.review.min_scout_diff_lines, 1000);
        assert_eq!(p.budget.release_notes_usd, 0.5);
        // dailyUsd is the machine's budget, not kept on the project (MACHINE_KEYS).
        assert_eq!(cfg.machine.budget.daily_usd, 0.0);
    }

    #[test]
    fn a_version_1_file_keeps_an_id_it_already_has() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"id":"already-named","repoPath":"C:/code/x"}"#,
        )
        .unwrap();
        assert_eq!(
            load(dir.path()).unwrap().projects[0].id,
            Some("already-named".to_string())
        );
    }

    #[test]
    fn galleries_default_to_models_and_ui_and_other_optional_fields_take_their_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"repoPath":"C:/code/x"}"#).unwrap();
        let cfg = load(dir.path()).unwrap();
        let p = &cfg.projects[0];
        assert_eq!(p.galleries.len(), 2);
        assert_eq!(p.galleries[0].name, "Models");
        assert_eq!(p.galleries[1].name, "UI");
        assert_eq!(p.max_conflict_rounds, 5);
        assert_eq!(p.max_check_rounds, 3);
        assert!(!p.require_ci);
        assert!(!p.ci_covers_checks);
        assert_eq!(p.deploy.staging, None);
        assert_eq!(p.phone, None);
    }

    #[test]
    fn a_broken_file_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{").unwrap();
        assert!(format!("{:#}", load(dir.path()).unwrap_err()).contains("config.json"));
    }

    #[test]
    fn the_data_dir_comes_from_the_env_or_the_working_folder() {
        assert_eq!(data_dir(no_env), PathBuf::from(".retaskable"));
        let env = |k: &str| (k == "RETASKABLE_DATA_DIR").then(|| "x".to_string());
        assert_eq!(data_dir(env), PathBuf::from("x"));
    }
}
