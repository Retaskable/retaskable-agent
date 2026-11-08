//! The extra allow entries a Cursor session gets from the project, and the check on ones a person
//! types (docs/specs/engines/06-permissions-the-allowlist.md sections 6.4 and 6.5):
//! `commandProgram`, `commandShells`, `allowEntryProblem` in `shared/cursorAllow.ts`.

use crate::config::{CheckConfig, GalleryConfig};

use super::allowlist::{DENY, outside_write};

/// Shells and launchers: allowing one would allow every command.
const LAUNCHERS: [&str; 11] = [
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

/// The first word of `cmd` that isn't a `NAME=value` assignment, quotes stripped.
pub(super) fn command_program(cmd: &str) -> Option<String> {
    cmd.split_whitespace()
        .find(|w| {
            !w.split_once('=').is_some_and(|(n, _)| {
                n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        })
        .map(|w| w.replace(['"', '\''], ""))
        .filter(|w| !w.is_empty())
}

/// `Shell(<program>)` for each check's then each gallery's command, once each, without denied
/// programs, shells or launchers.
pub(super) fn command_shells(checks: &[CheckConfig], galleries: &[GalleryConfig]) -> Vec<String> {
    let commands = checks
        .iter()
        .map(|c| &c.command)
        .chain(galleries.iter().map(|g| &g.command));
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
    if let Some(program) = entry.strip_prefix("Shell(").and_then(|r| r.strip_suffix(')')) {
        if is_launcher(program) {
            return Some(format!("{entry} would allow every command"));
        }
        if is_denied(program) {
            return Some(format!("{} is always denied", base_name(program)));
        }
        return None;
    }
    if entry.starts_with("Write(") && entry.ends_with(')') {
        return outside_write(entry).then(|| format!("{entry} writes outside the workspace"));
    }
    if entry.starts_with("Read(") && entry.ends_with(')') {
        return None;
    }
    Some(format!("{entry} isn't Shell(…), Read(…) or Write(…)"))
}

/// The typed entries, split on commas and new lines, trimmed, empties dropped.
pub(super) fn split_allow_entries(text: &str) -> Vec<String> {
    text.split([',', '\n'])
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests;
