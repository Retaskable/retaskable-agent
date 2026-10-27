//! Guessing a new project's type, checks and play command from its own repo (docs/specs/agent/09-web-ui.md
//! section 9.1, "The scan"; [project-types.md](https://github.com/Retaskable/Retaskable.com/blob/main/docs/specs/project-types.md)
//! sections 2 and 5), ported from `detectProjectType` and `detectChecks` in Retaskable.com's
//! `agent/src/setup.ts`. Pure and synchronous: it only reads files already checked out, runs
//! nothing, and whatever calls it (the Setup screen's own suggestion, a later piece) decides
//! whether to use what it finds. Nothing in this crate calls it yet. Whether the picked base
//! branch's workflows cover it (`hasWorkflows`'s other half, team-branch.md section 3.2) is
//! `crate::ci_coverage` instead: that reads a specific git ref, not the checked-out files here.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use crate::config::{CheckConfig, ProjectType};

static GAME_CRATES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(?:\[dependencies\.)?(bevy|macroquad|ggez|piston)\s*[=.\]]").unwrap());
static BIN_CRATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*\[\[bin\]\]").unwrap());
static EXPOSES_PORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?im)^\s*EXPOSE\s").unwrap());

const WEB_DEPS: [&str; 6] = ["vite", "next", "react", "vue", "svelte", "@angular/core"];
const SERVER_DEPS: [&str; 4] = ["express", "fastify", "koa", "hono"];

/// Guesses the project type from the repo's files; the first match wins (project-types.md section 2).
pub fn detect_project_type(repo: &Path) -> ProjectType {
    let cargo = read_repo_file(repo, "Cargo.toml");
    let pkgs = package_jsons(repo);
    let deps = deps_of(&pkgs);

    if GAME_CRATES.is_match(&cargo) || repo.join("project.godot").exists() || repo.join("ProjectSettings").exists() {
        return ProjectType::Game;
    }
    if WEB_DEPS.iter().any(|d| deps.contains(*d)) {
        return ProjectType::Web;
    }
    if SERVER_DEPS.iter().any(|d| deps.contains(*d))
        || repo.join("wrangler.toml").exists()
        || repo.join("wrangler.jsonc").exists()
        || EXPOSES_PORT.is_match(&read_repo_file(repo, "Dockerfile"))
    {
        return ProjectType::Server;
    }
    if pkgs.iter().any(|p| !p.bin.is_null()) || BIN_CRATE.is_match(&cargo) {
        return ProjectType::Cli;
    }
    ProjectType::Other
}

/// `detectChecks`'s suggestion: a check per command it recognises in the repo's own manifest, and
/// a play command.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DetectedChecks {
    pub checks: Vec<CheckConfig>,
    pub play_command: String,
}

/// Suggests checks and a play command from the repo's own manifest; an npm `launch` script wins
/// over `start` (project-types.md section 5, "Retaskable's own repo").
pub fn detect_checks(repo: &Path) -> DetectedChecks {
    if repo.join("Cargo.toml").exists() {
        return DetectedChecks {
            checks: vec![
                check("test", "cargo test", true, 1800),
                check("clippy", "cargo clippy --all-targets", false, 900),
                check("fmt", "cargo fmt --check", false, 120),
            ],
            play_command: "cargo run --release".to_string(),
        };
    }
    let Some(pkg) = load_package(repo) else {
        return DetectedChecks::default();
    };
    let mut checks = Vec::new();
    if has_script(&pkg, "typecheck") {
        checks.push(check("typecheck", "npm run typecheck", true, 600));
    }
    if has_script(&pkg, "test") {
        checks.push(check("test", "npm test", true, 1200));
    }
    if has_script(&pkg, "lint") {
        checks.push(check("lint", "npm run lint", false, 600));
    }
    let play_command = if has_script(&pkg, "launch") {
        "npm run launch".to_string()
    } else if has_script(&pkg, "start") {
        "npm start".to_string()
    } else {
        String::new()
    };
    DetectedChecks { checks, play_command }
}

/// Whether the repo's checkout has any GitHub Actions workflow file (`suggest`'s `hasWorkflows`,
/// docs/specs/agent/09-web-ui.md section 9.1, "The scan").
pub fn has_workflows(repo: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(repo.join(".github").join("workflows")) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|n| n.ends_with(".yml") || n.ends_with(".yaml"))
    })
}

fn check(name: &str, command: &str, blocking: bool, timeout_sec: u64) -> CheckConfig {
    CheckConfig {
        name: name.to_string(),
        command: command.to_string(),
        blocking,
        timeout_sec,
    }
}

fn read_repo_file(repo: &Path, name: &str) -> String {
    std::fs::read_to_string(repo.join(name)).unwrap_or_default()
}

/// A root `package.json`'s shape, read only for the fields the scan looks at. Every field is a raw
/// `Value` rather than a typed map: an unexpected shape (`"dependencies": null`, a non-string
/// script) then reads as empty/absent for that field alone, the same as the TS original, rather
/// than failing to parse the whole package and losing its other fields.
#[derive(Debug, Deserialize, Default)]
struct PackageJson {
    #[serde(default)]
    dependencies: Value,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: Value,
    #[serde(default)]
    bin: Value,
    #[serde(default)]
    workspaces: Value,
    #[serde(default)]
    scripts: Value,
}

/// `dir`'s `package.json`, or `None` when it's missing or isn't valid JSON (read, not guessed past).
fn load_package(dir: &Path) -> Option<PackageJson> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// An object's own key names, or none when it isn't an object.
fn keys_of(v: &Value) -> Vec<String> {
    match v {
        Value::Object(map) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

/// A JSON value's strings, when it's an array of them; anything else is none.
fn strings_in(v: &Value) -> Vec<String> {
    match v {
        Value::Array(arr) => arr.iter().filter_map(|s| s.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

/// The npm `workspaces` field's entries: a plain array of them, or (the object form) its
/// `packages` array; anything else is none.
fn workspace_entries(pkg: &PackageJson) -> Vec<String> {
    match &pkg.workspaces {
        v @ Value::Array(_) => strings_in(v),
        Value::Object(obj) => obj.get("packages").map(strings_in).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Whether `pkg.scripts[name]` is set to something truthy, the loose JS truthiness `detectChecks`
/// tests (`if (scripts.typecheck)`): missing, `null`, `false` or an empty string don't count.
fn has_script(pkg: &PackageJson, name: &str) -> bool {
    match pkg.scripts.get(name) {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// The repo root and its npm workspaces' folders (a `dir/*` entry lists that folder's own
/// subdirectories); none of it without a root `package.json`.
fn workspace_dirs(repo: &Path) -> Vec<PathBuf> {
    let Some(root) = load_package(repo) else {
        return Vec::new();
    };
    let mut dirs = Vec::new();
    for entry in workspace_entries(&root) {
        if let Some(parent) = entry.strip_suffix("/*") {
            if let Ok(read) = std::fs::read_dir(repo.join(parent)) {
                dirs.extend(read.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
            }
        } else {
            dirs.push(repo.join(&entry));
        }
    }
    std::iter::once(repo.to_path_buf()).chain(dirs).collect()
}

/// The root `package.json` and its workspaces'.
fn package_jsons(repo: &Path) -> Vec<PackageJson> {
    workspace_dirs(repo)
        .iter()
        .filter_map(|dir| load_package(dir))
        .collect()
}

/// Every dependency and devDependency name across a set of `package.json`s.
fn deps_of(pkgs: &[PackageJson]) -> HashSet<String> {
    pkgs.iter()
        .flat_map(|p| keys_of(&p.dependencies).into_iter().chain(keys_of(&p.dev_dependencies)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp repo with each path's content written (parent folders made as needed).
    fn repo(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, body) in files {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, body).unwrap();
        }
        dir
    }

    fn pkg(json: &str) -> String {
        json.to_string()
    }

    #[test]
    fn a_cargo_toml_depending_on_a_game_crate_is_a_game() {
        let dir = repo(&[("Cargo.toml", "[dependencies]\nbevy = \"0.14\"\n")]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Game);
    }

    #[test]
    fn a_dependencies_dot_table_game_crate_wins_over_a_bin_crate() {
        let dir = repo(&[(
            "Cargo.toml",
            "[dependencies.macroquad]\nversion = \"0.4\"\n[[bin]]\nname = \"x\"\n",
        )]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Game);
    }

    #[test]
    fn a_project_godot_file_is_a_game() {
        let dir = repo(&[("project.godot", "")]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Game);
    }

    #[test]
    fn a_web_dependency_wins_over_a_server_one() {
        let dir = repo(&[(
            "package.json",
            &pkg(r#"{"dependencies":{"react":"^19","express":"^5"}}"#),
        )]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Web);
    }

    #[test]
    fn a_web_dependency_in_an_npm_workspace_folder_is_found() {
        // Retaskable's own shape: the web app lives in a workspace, next to a Worker.
        let dir = repo(&[
            (
                "package.json",
                &pkg(r#"{"workspaces":["agent","web","cloud"],"devDependencies":{"concurrently":"^9"}}"#),
            ),
            ("web/package.json", &pkg(r#"{"devDependencies":{"vite":"^6"}}"#)),
            ("cloud/wrangler.toml", ""),
        ]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Web);
    }

    #[test]
    fn a_server_dependency_in_a_dir_star_workspace_is_found() {
        let dir = repo(&[
            ("package.json", &pkg(r#"{"workspaces":["packages/*"]}"#)),
            ("packages/api/package.json", &pkg(r#"{"dependencies":{"hono":"^4"}}"#)),
        ]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Server);
    }

    #[test]
    fn a_wrangler_jsonc_with_no_package_json_is_a_server() {
        let dir = repo(&[("wrangler.jsonc", "{}")]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Server);
    }

    #[test]
    fn a_dockerfile_that_exposes_a_port_is_a_server() {
        let dir = repo(&[("Dockerfile", "FROM node\nEXPOSE 8080\n")]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Server);
    }

    #[test]
    fn a_dockerfile_with_no_expose_matches_nothing() {
        let dir = repo(&[("Dockerfile", "FROM node\n")]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Other);
    }

    #[test]
    fn a_package_json_bin_field_is_a_cli() {
        let dir = repo(&[("package.json", &pkg(r#"{"bin":{"x":"x.js"}}"#))]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Cli);
    }

    #[test]
    fn a_cargo_toml_bin_crate_with_no_game_crate_is_a_cli() {
        let dir = repo(&[("Cargo.toml", "[package]\nname = \"x\"\n\n[[bin]]\nname = \"x\"\n")]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Cli);
    }

    #[test]
    fn a_repo_matching_nothing_is_other() {
        let dir = repo(&[("README.md", "")]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Other);
    }

    #[test]
    fn a_cargo_project_gets_the_fixed_rust_checks_and_play_command() {
        let dir = repo(&[("Cargo.toml", "[package]\nname = \"x\"\n")]);
        let found = detect_checks(dir.path());
        assert_eq!(
            found.checks,
            vec![
                check("test", "cargo test", true, 1800),
                check("clippy", "cargo clippy --all-targets", false, 900),
                check("fmt", "cargo fmt --check", false, 120),
            ]
        );
        assert_eq!(found.play_command, "cargo run --release");
    }

    #[test]
    fn an_npm_launch_script_is_suggested_over_start() {
        let dir = repo(&[("package.json", &pkg(r#"{"scripts":{"start":"node ."}}"#))]);
        assert_eq!(detect_checks(dir.path()).play_command, "npm start");

        // This repo's shape: npm start would be a second agent, so launch serves the UI on mock data.
        let dir = repo(&[(
            "package.json",
            &pkg(r#"{"scripts":{"start":"node .","launch":"npm run launch -w web","test":"x"}}"#),
        )]);
        let found = detect_checks(dir.path());
        assert_eq!(found.play_command, "npm run launch");
        assert_eq!(found.checks, vec![check("test", "npm test", true, 1200)]);

        let dir = repo(&[("package.json", &pkg(r#"{"scripts":{"test":"x"}}"#))]);
        assert_eq!(detect_checks(dir.path()).play_command, "");
    }

    #[test]
    fn npm_scripts_each_suggest_their_own_check() {
        let dir = repo(&[(
            "package.json",
            &pkg(r#"{"scripts":{"typecheck":"tsc","test":"vitest","lint":"eslint ."}}"#),
        )]);
        assert_eq!(
            detect_checks(dir.path()).checks,
            vec![
                check("typecheck", "npm run typecheck", true, 600),
                check("test", "npm test", true, 1200),
                check("lint", "npm run lint", false, 600),
            ]
        );
    }

    #[test]
    fn a_repo_with_no_manifest_suggests_nothing() {
        let dir = repo(&[("README.md", "")]);
        assert_eq!(detect_checks(dir.path()), DetectedChecks::default());
    }

    #[test]
    fn a_script_set_to_an_empty_string_suggests_nothing() {
        let dir = repo(&[("package.json", &pkg(r#"{"scripts":{"lint":"","launch":""}}"#))]);
        let found = detect_checks(dir.path());
        assert_eq!(found.checks, Vec::new());
        assert_eq!(found.play_command, "");
    }

    #[test]
    fn a_repo_with_a_github_workflow_file_has_workflows() {
        let dir = repo(&[(".github/workflows/ci.yml", "on: push\n")]);
        assert!(has_workflows(dir.path()));
    }

    #[test]
    fn a_repo_with_no_workflows_dir_does_not_have_workflows() {
        let dir = repo(&[("README.md", "")]);
        assert!(!has_workflows(dir.path()));
    }

    #[test]
    fn a_workflows_dir_with_only_non_yaml_files_does_not_have_workflows() {
        let dir = repo(&[(".github/workflows/README.md", "")]);
        assert!(!has_workflows(dir.path()));
    }

    #[test]
    fn an_unexpected_shape_on_one_field_doesnt_lose_a_packages_other_fields() {
        // A malformed `dependencies` (null, not an object) still leaves `bin` and `workspaces` readable.
        let dir = repo(&[(
            "package.json",
            &pkg(r#"{"dependencies":null,"bin":{"x":"x.js"},"scripts":{"test":123}}"#),
        )]);
        assert_eq!(detect_project_type(dir.path()), ProjectType::Cli);
        // A non-string, non-null, non-false script value is still truthy, like a JS object/number is.
        assert_eq!(
            detect_checks(dir.path()).checks,
            vec![check("test", "npm test", true, 1200)]
        );
    }
}
