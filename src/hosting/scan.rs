//! What Go live's picker reads from the repo (`detectHosting` in `agent/src/setup.ts`, go-live.md section 3): a back
//! end (a server dependency, a Worker's `main`, an API folder, a Dockerfile that EXPOSEs a port, a Python server) and
//! whether it's a phone or desktop app. Reads files only.
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use super::App;
use crate::scan::{EXPOSES_PORT, SERVER_DEPS, deps_of, package_jsons, read_repo_file};

/// A Worker's entry: a `main` key in `wrangler.toml` or `wrangler.jsonc` (a Pages-only one has none).
static WORKER_MAIN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?m)(?:^|[{,])\s*"?main"?\s*[=:]"#).unwrap());
static PYTHON_SERVERS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?im)^\s*(flask|django|fastapi)\b").unwrap());

/// Folders that hold a back end, one list of path components each (never a `/` joined string).
const BACK_END_DIRS: [&[&str]; 8] = [
    &["api"],
    &["server"],
    &["functions"],
    &["netlify", "functions"],
    &["pages", "api"],
    &["app", "api"],
    &["src", "pages", "api"],
    &["src", "app", "api"],
];

/// What the repo scan found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub back_end: bool,
    pub app: Option<App>,
}

fn has_dir(repo: &Path, parts: &[&str]) -> bool {
    repo.join(parts.iter().collect::<std::path::PathBuf>()).exists()
}

pub fn detect_hosting(repo: &Path) -> Found {
    let deps = deps_of(&package_jsons(repo));
    let wrangler = format!(
        "{}\n{}",
        read_repo_file(repo, "wrangler.toml"),
        read_repo_file(repo, "wrangler.jsonc")
    );
    let back_end = SERVER_DEPS.iter().any(|d| deps.contains(*d))
        || WORKER_MAIN.is_match(&wrangler)
        || EXPOSES_PORT.is_match(&read_repo_file(repo, "Dockerfile"))
        || PYTHON_SERVERS.is_match(&read_repo_file(repo, "requirements.txt"))
        || BACK_END_DIRS.iter().any(|d| has_dir(repo, d));
    let app = if deps.contains("electron") || deps.contains("@tauri-apps/api") || repo.join("src-tauri").exists() {
        Some(App::Desktop)
    } else if deps.contains("expo") || deps.contains("react-native") {
        Some(App::Mobile)
    } else {
        None
    };
    Found { back_end, app }
}
