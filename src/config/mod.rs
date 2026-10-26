//! `.retaskable/config.json`, read the same way as the TypeScript agent (Retaskable.com agent/src/config.ts and
//! paths.ts), following docs/specs/agent/10-config-retaskable-config-json.md: the machine's settings at the top
//! and each project's in `projects[]` (version 2), or the whole file read as one project (version 1, no
//! `"version"` key). `engines`, `cursor` and `github` are kept as raw JSON for now: their own specs (engines.md,
//! github-workflow.md) aren't ported to this agent yet. Like the TypeScript agent, nothing here validates a
//! project's fields at parse time — a value missing from a hand-edited or partial file just reads as empty.
//!
//! Split by responsibility: this file reads the whole file and ties its two halves together; `machine` holds
//! the settings shared by every project here (docs/specs/agent-settings.md); `project` holds each project's own.
mod machine;
mod project;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

pub use machine::{
    CursorModels, Engine, EngineChoiceConfig, LaneEngine, MachineConfig, cursor_models, engine_choice, lanes,
    local_model, poll_interval_sec, port,
};
pub use project::ProjectConfig;

/// The settings file: the machine's own settings, shared by every project here, and each project's.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfigFile {
    pub machine: MachineConfig,
    pub projects: Vec<ProjectConfig>,
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
    let mut machine: MachineConfig = serde_json::from_value(raw.clone())?;
    if raw.get("version").and_then(Value::as_u64) == Some(2) {
        // TypeScript's migrateConfig treats a `projects` that isn't an array as `[]`.
        let projects_raw: Vec<Value> = match raw.get("projects") {
            Some(Value::Array(arr)) => arr.clone(),
            _ => Vec::new(),
        };
        if raw.get("lanes").is_none() {
            machine.lanes = fallback_lanes(&projects_raw);
        }
        let projects = projects_raw
            .into_iter()
            .map(|v| serde_json::from_value(merge_migrated(v)))
            .collect::<std::result::Result<Vec<ProjectConfig>, _>>()?;
        return Ok(ConfigFile { machine, projects });
    }
    let mut project: ProjectConfig = serde_json::from_value(raw)?;
    if project.id.is_none() {
        project.id = Some(project::project_id(&project.repo_path));
    }
    Ok(ConfigFile {
        machine,
        projects: vec![project],
    })
}

/// A version 2 project block whose settings have moved to the shared backlog keeps only the computer's own
/// fields; everything else sits under `migrated` (shared/configTypes.ts `ProjectConfig`). Read it merged, the
/// block's own keys over the migrated ones, as `projectConfig` in agent/src/config.ts does.
fn merge_migrated(mut value: Value) -> Value {
    let Value::Object(obj) = &mut value else {
        return value;
    };
    let Some(Value::Object(migrated)) = obj.remove("migrated") else {
        return value;
    };
    let mut merged = migrated;
    merged.extend(obj.iter().map(|(k, v)| (k.clone(), v.clone())));
    Value::Object(merged)
}

/// agent-settings.md section 3.1 (TypeScript's `projectLanes`): when the top-level `lanes` is missing, the
/// computer's lanes is the largest whole number of 1 or more found in any project's own `lanes` or
/// `migrated.lanes`, and the default only when there is none.
fn fallback_lanes(projects: &[Value]) -> u32 {
    projects
        .iter()
        .flat_map(|p| [p.get("lanes"), p.get("migrated").and_then(|m| m.get("lanes"))])
        .filter_map(|v| v.and_then(Value::as_u64))
        .filter(|&n| n >= 1)
        .max()
        .map(|n| n as u32)
        .unwrap_or_else(|| MachineConfig::default().lanes)
}

#[cfg(test)]
mod tests {
    use super::machine::DEFAULT_PORT;
    use super::project::{Playtests, ProjectType, SandboxMode};
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
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
    fn a_migrated_project_block_reads_its_settings_from_migrated() {
        // Once a project's settings move to the shared backlog, its `projects[]` block keeps only the
        // computer's own fields; everything else (baseBranch, checks, ...) sits under `migrated`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{
                "version": 2,
                "projects": [
                    { "id": "x", "repoPath": "C:/code/x",
                      "migrated": { "baseBranch": "main", "checks": [{ "name": "test", "command": "cargo test" }] } }
                ]
            }"#,
        )
        .unwrap();
        let cfg = load(dir.path()).unwrap();
        let p = &cfg.projects[0];
        assert_eq!(p.base_branch, "main");
        assert_eq!(p.checks.len(), 1);
        assert_eq!(p.checks[0].command, "cargo test");
    }

    #[test]
    fn a_project_blocks_own_key_wins_over_its_migrated_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{
                "version": 2,
                "projects": [
                    { "id": "x", "repoPath": "C:/code/x", "baseBranch": "develop",
                      "migrated": { "baseBranch": "main" } }
                ]
            }"#,
        )
        .unwrap();
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.projects[0].base_branch, "develop");
    }

    #[test]
    fn the_machines_lanes_falls_back_to_the_largest_a_project_or_its_migrated_block_had() {
        // agent-settings.md section 3.1: no top-level `lanes` means the computer's lanes is the largest
        // whole number of 1 or more found in any project's own `lanes` or `migrated.lanes`.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{
                "version": 2,
                "projects": [
                    { "id": "a", "repoPath": "C:/code/a", "lanes": 3 },
                    { "id": "b", "repoPath": "C:/code/b", "migrated": { "lanes": 5 } }
                ]
            }"#,
        )
        .unwrap();
        assert_eq!(load(dir.path()).unwrap().machine.lanes, 5);
    }

    #[test]
    fn the_machines_lanes_is_the_default_when_no_project_has_one_either() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"version": 2, "projects": [{ "id": "a", "repoPath": "C:/code/a" }]}"#,
        )
        .unwrap();
        assert_eq!(load(dir.path()).unwrap().machine.lanes, 2);
    }

    #[test]
    fn a_version_2_files_top_level_lanes_is_not_overridden_by_a_projects() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"version": 2, "lanes": 7, "projects": [{ "id": "a", "repoPath": "C:/code/a", "lanes": 3 }]}"#,
        )
        .unwrap();
        assert_eq!(load(dir.path()).unwrap().machine.lanes, 7);
    }

    #[test]
    fn a_non_array_projects_in_a_version_2_file_reads_as_no_projects() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), r#"{"version": 2, "projects": null}"#).unwrap();
        assert!(load(dir.path()).unwrap().projects.is_empty());
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
    fn an_unknown_engine_in_config_json_does_not_stop_the_agent_starting() {
        // "pi" is a real engine (ENGINE_PICKS in the TypeScript agent, engines.md section 13), and
        // an unrecognised one ("steam") in a role or a lane must not fail the whole load either.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{
                "engineChoice": {
                    "default": "pi",
                    "roles": { "implementer": "steam" },
                    "lanes": ["claude", "steam"]
                }
            }"#,
        )
        .unwrap();
        let cfg = load(dir.path()).unwrap();
        assert_eq!(cfg.machine.engine_choice.default, Some(Engine::Pi));
        assert_eq!(cfg.machine.engine_choice.roles.implementer, None);
        assert!(cfg.machine.engine_choice.lanes.is_empty());
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
