//! What Add project reads off a checkout and its folder rules: the backlog file, the id, the worktrees folder, and the
//! scrub that keeps a token out of every message.
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::{Result, bail};
use regex::Regex;

use crate::config::{ConfigFile, ProjectConfig, ProjectType};
use crate::env::Env;

/// `BACKLOG_NAMES` in `agent/src/setup.ts`, in the order it prefers them.
const BACKLOG_NAMES: [&str; 6] = [
    "docs/specs/build-order.md",
    "ROADMAP.md",
    "docs/ROADMAP.md",
    "docs/roadmap.md",
    "BACKLOG.md",
    "docs/BACKLOG.md",
];

/// The checkout's tracked Markdown files, sorted.
fn markdown_files(repo: &Path) -> Vec<String> {
    let Ok(out) = crate::git_bin::command(repo).args(["ls-files", "*.md"]).output() else {
        return Vec::new();
    };
    let mut files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    files.sort();
    files
}

/// Where the project keeps its list of work, as `suggest` and `linkPlanned` pick it: a well-known name, else a
/// Markdown file named like a build order, roadmap or backlog, else the first Markdown file.
pub fn backlog_file(repo: &Path) -> Option<String> {
    pick_backlog(&markdown_files(repo))
}

pub(super) fn pick_backlog(files: &[String]) -> Option<String> {
    static LIKE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)build-order|roadmap|backlog").unwrap());
    BACKLOG_NAMES
        .iter()
        .find(|n| files.iter().any(|f| f == *n))
        .map(|n| n.to_string())
        .or_else(|| files.iter().find(|f| LIKE.is_match(f)).cloned())
        .or_else(|| files.first().cloned())
}

/// The lanes' folder, beside the repo (`defaultWorktreesDir` in `agent/src/config.ts`).
pub fn default_worktrees_dir(repo: &Path) -> PathBuf {
    let name = repo.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    repo.parent()
        .unwrap_or(Path::new(""))
        .join(".retaskable-worktrees")
        .join(name)
}

/// The id config would derive from the folder's name, made different from every existing one with `-2`, `-3`...
pub fn unique_id(repo: &Path, existing: &[ProjectConfig]) -> String {
    let base = ProjectConfig {
        repo_path: repo.to_string_lossy().into_owned(),
        ..Default::default()
    }
    .id_or_derived();
    let taken = |id: &str| existing.iter().any(|p| p.id_or_derived() == id);
    let mut id = base.clone();
    let mut n = 2;
    while taken(&id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    id
}

pub fn type_name(t: ProjectType) -> &'static str {
    match t {
        ProjectType::Game => "game",
        ProjectType::Web => "web",
        ProjectType::Server => "server",
        ProjectType::Cli => "cli",
        ProjectType::Other => "other",
    }
}

/// `folderProblem` in `agent/src/github.ts`: why a clone can't go in `dest`.
pub fn folder_problem(dest: &Path, data_dir: &Path, projects: &[ProjectConfig]) -> Result<()> {
    if !dest.is_absolute() {
        bail!("The folder must be a full path.");
    }
    if dest.starts_with(data_dir) {
        bail!("The folder can't be inside Retaskable's own data folder.");
    }
    let inside_worktrees = projects.iter().any(|p| {
        let wt = if p.worktrees_dir.is_empty() {
            default_worktrees_dir(Path::new(&p.repo_path))
        } else {
            PathBuf::from(&p.worktrees_dir)
        };
        dest.starts_with(wt)
    });
    if inside_worktrees {
        bail!("The folder can't be inside a project's worktrees folder.");
    }
    if dest.exists() {
        let empty = dest.is_dir() && std::fs::read_dir(dest).map(|mut d| d.next().is_none()).unwrap_or(false);
        if !empty {
            bail!(
                "{} already exists and isn't a copy of this repo: move it, or clone the repo somewhere else first.",
                dest.display()
            );
        }
    }
    Ok(())
}

/// A version 1 file (no `"version"`) is one project in the file's own top level; adding a second needs the file moved to
/// version 2 first, which this doesn't do for a person. Refused, not guessed at.
pub fn refuse_old_format(dir: &Path, cfg: &ConfigFile) -> Result<()> {
    let text = std::fs::read_to_string(dir.join("config.json")).unwrap_or_default();
    let raw: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    if raw.is_object()
        && raw.get("version").is_none()
        && !cfg.projects.is_empty()
        && !cfg.projects[0].repo_path.is_empty()
    {
        bail!("config.json is in the old one-project format: update it to version 2 before adding a project.");
    }
    Ok(())
}

static TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:gh[pousr]_|github_pat_)[A-Za-z0-9_]{8,}|x-access-token:[^@\s]+").unwrap());

/// `text` with every GitHub token in it taken out: a token's own shape, and the value of each token variable this
/// process holds.
pub fn redact(text: &str, env: &Env) -> String {
    let mut out = TOKEN.replace_all(text, "[token]").into_owned();
    for var in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_ENTERPRISE_TOKEN",
        "GITHUB_ENTERPRISE_TOKEN",
    ] {
        if let Some(value) = env.get(var).filter(|v| v.len() >= 6) {
            out = out.replace(&value, "[token]");
        }
    }
    out
}

/// A checkout of `repo` among `parent`'s subfolders, by its `origin` (as `findCheckouts` in `agent/src/github.ts`:
/// case doesn't matter in a GitHub name), the first by folder name.
pub fn find_checkout(parent: &Path, repo: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(parent)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join(".git").exists())
        .collect();
    dirs.sort();
    dirs.into_iter().find(|d| {
        crate::repo::origin_url(d)
            .and_then(|url| crate::repo::remote_repo(&url))
            .is_some_and(|found| found.eq_ignore_ascii_case(repo))
    })
}
