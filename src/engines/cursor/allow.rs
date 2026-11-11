//! The extra allow entries a Cursor session gets from the project, and the check on ones a person
//! types (docs/specs/engines/06-permissions-the-allowlist.md sections 6.4 and 6.5):
//! `commandProgram`, `commandShells`, `allowEntryProblem` in `shared/cursorAllow.ts`.

use std::sync::LazyLock;

use regex::Regex;

use crate::config::{CheckConfig, ProjectConfig};
use crate::engines::engine::CursorOptions;

use super::allowlist::{DENY, outside_write};

/// Shells and launchers: allowing one would allow every command.
const LAUNCHERS: [&str; 12] = [
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "fish",
    "cmd",
    "powershell",
    "pwsh",
    "env",
    "xargs",
    "rtk",
];

/// A program's base name, without `.exe`, lower case; split on both `/` and `\` on any host.
fn base_name(program: &str) -> String {
    let name = program.rsplit(['/', '\\']).next().unwrap_or("").to_lowercase();
    name.strip_suffix(".exe").unwrap_or(&name).to_string()
}

fn is_denied(program: &str) -> bool {
    let name = base_name(program);
    DENY.iter()
        .filter_map(|e| e.strip_prefix("Shell(").and_then(|r| r.strip_suffix(')')))
        .any(|d| d == name)
}

fn is_launcher(program: &str) -> bool {
    LAUNCHERS.contains(&base_name(program).as_str())
}

/// The first word of `cmd` that isn't a `NAME=value` assignment, with its outer quotes stripped.
/// A quoted run stays part of one word, so `FOO="a b" cargo test` gives `cargo`.
pub(super) fn command_program(cmd: &str) -> Option<String> {
    static WORD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"(?:[^\s"']+|"[^"]*"|'[^']*')+"#).expect("word pattern"));
    static ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\w+=").expect("assignment pattern"));
    WORD.find_iter(cmd)
        .map(|m| m.as_str())
        .find(|w| !ASSIGNMENT.is_match(w))
        .map(|w| {
            let w = w.strip_prefix(['"', '\'']).unwrap_or(w);
            w.strip_suffix(['"', '\'']).unwrap_or(w).to_string()
        })
        .filter(|w| !w.is_empty())
}

/// `Shell(<program>)` for each check's command, once each, without denied programs, shells or launchers.
pub(crate) fn command_shells(checks: &[CheckConfig]) -> Vec<String> {
    let commands = checks.iter().map(|c| &c.command);
    let mut out: Vec<String> = Vec::new();
    for program in commands.filter_map(|c| command_program(c.as_str())) {
        if is_denied(&program) || is_launcher(&program) {
            continue;
        }
        let entry = format!("Shell({program})");
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    out
}

/// Why a typed entry can't be allowed, or none when it's fine.
pub(super) fn allow_entry_problem(entry: &str) -> Option<String> {
    let entry = entry.trim();
    if entry.starts_with("Shell(*") {
        return Some("Shell(*) would allow every command".to_string());
    }
    let inner = |kind: &str| {
        entry
            .strip_prefix(kind)
            .and_then(|r| r.strip_suffix(')'))
            .filter(|r| !r.is_empty())
    };
    if let Some(program) = inner("Shell(") {
        if is_launcher(program) {
            return Some(format!("{entry} would allow every command"));
        }
        if is_denied(program) {
            return Some(format!("{program} is always denied"));
        }
        return None;
    }
    if inner("Write(").is_some() {
        return outside_write(entry).then(|| format!("{entry} writes outside the workspace"));
    }
    if inner("Read(").is_some() {
        return None;
    }
    Some(format!("{entry} isn't Shell(…), Read(…) or Write(…)"))
}

/// The code host's CLI for a remote URL: `gh` on GitHub, `glab` on GitLab, none for anything else.
fn host_cli_of(url: &str) -> Option<&'static str> {
    let url = url.to_lowercase();
    if url.contains("github.com") {
        Some("gh")
    } else if url.contains("gitlab") {
        Some("glab")
    } else {
        None
    }
}

/// Today's options for the implementer and trial sessions with the flag Off: `gh` when the project names a repo.
pub fn with_project_or_gh(project: &ProjectConfig, base: CursorOptions) -> CursorOptions {
    if crate::tiers::cursor_allow_derive_on() {
        return with_project(project, base);
    }
    CursorOptions {
        cli: (!project.github_repo.is_empty()).then(|| "gh".to_string()),
        ..base
    }
}

/// With the `cursor-allow-derive` flag On, `base` (the model `place` set) with the project's allowlist extras and code host's CLI, read from
/// `project` as it is now (`hostCli`). Read-only roles are given the same; `prepare_cursor` ignores the
/// extras for them. The host comes from the repo's `origin`, else GitHub when the project names a repo.
pub fn with_project(project: &ProjectConfig, base: CursorOptions) -> CursorOptions {
    if !crate::tiers::cursor_allow_derive_on() {
        return base;
    }
    let mut allow = command_shells(&project.checks);
    let typed = project
        .engines
        .as_ref()
        .and_then(|e| e.get("allow"))
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|e| e.as_str())
        .map(str::trim)
        .filter(|e| allow_entry_problem(e).is_none());
    for entry in typed {
        if !allow.iter().any(|a| a == entry) {
            allow.push(entry.to_string());
        }
    }
    let origin = (!project.repo_path.is_empty())
        .then(|| crate::repo::origin_url(std::path::Path::new(&project.repo_path)))
        .flatten();
    let cli = match origin {
        Some(url) => host_cli_of(&url),
        None => (!project.github_repo.is_empty()).then_some("gh"),
    };
    CursorOptions {
        allow,
        cli: cli.map(String::from),
        ..base
    }
}

/// The typed entries, split on commas and new lines, trimmed, empties dropped.
pub(crate) fn split_allow_entries(text: &str) -> Vec<String> {
    text.split([',', '\n'])
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(String::from)
        .collect()
}

/// Why typed `engines.allow` text can't be saved: "Cursor can't be allowed this: <reason>" for
/// the first entry `allow_entry_problem` refuses, or none when every entry is fine.
pub(crate) fn engines_allow_error(entries: &[String]) -> Option<String> {
    entries
        .iter()
        .find_map(|e| allow_entry_problem(e))
        .map(|reason| format!("Cursor can't be allowed this: {reason}"))
}

#[cfg(test)]
mod tests;
