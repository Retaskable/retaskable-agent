//! Continuing a Claude Code session (docs/specs/expert-chat.md section 3.10; Retaskable.com's
//! `agent/src/expert/claudeSessions.ts`): the person's recent sessions on this computer, read from Claude's own
//! session files for `claude_sessions`, and the handover a fresh expert session starts from. Neither carries the
//! conversation off the computer: the list is titles and folders, and the handover stays in the lane.
//!
//! This file lists the sessions and reads a person's words out of a line; `handover.rs` builds the first message of
//! a continuing session and `branch.rs` starts its branch at the tip of the folder's.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::{Value, json};

use super::title;
use crate::env::Env;

mod branch;
mod handover;

pub use branch::branch_at_tip;
pub use handover::{Continued, continuing, handover};

/// The most sessions the list carries, and the most files looked at for them, newest first.
const LIST: usize = 20;
const LOOK_AT: usize = 200;
/// Each file's start (its first message) and end (its latest title, folder and branch) are read, not the whole.
const HEAD_BYTES: u64 = 64_000;
const TAIL_BYTES: u64 = 256_000;

/// Claude Code's folder of session files: `projects` under its config folder (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
pub fn projects_root(env: &Env) -> PathBuf {
    let config = env.get("CLAUDE_CONFIG_DIR").map(PathBuf::from).unwrap_or_else(|| {
        let home = env.get("HOME").or_else(|| env.get("USERPROFILE")).unwrap_or_default();
        PathBuf::from(home).join(".claude")
    });
    config.join("projects")
}

/// One of the computer's Claude Code sessions, as `claude_sessions` lists it (the id, title, folder, branch and when
/// it was last used, and whether its folder has uncommitted changes), never its conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeSession {
    pub id: String,
    pub title: String,
    pub folder: String,
    pub branch: Option<String>,
    /// Milliseconds since the epoch.
    pub at: u64,
    pub uncommitted: bool,
}

impl ClaudeSession {
    pub fn to_json(&self) -> Value {
        let mut s = json!({ "id": self.id, "title": self.title, "folder": self.folder, "at": self.at });
        if let Some(b) = &self.branch {
            s["branch"] = json!(b);
        }
        if self.uncommitted {
            s["uncommitted"] = json!(true);
        }
        s
    }
}

/// What Claude Code adds around the person's words: reminders, the editor's selection, slash commands' plumbing.
fn noise_tag(name: &str) -> bool {
    let rest = |prefix: &str, ok: fn(char) -> bool| {
        name.strip_prefix(prefix)
            .is_some_and(|r| !r.is_empty() && r.chars().all(ok))
    };
    name == "system-reminder"
        || rest("ide_", |c| c.is_ascii_lowercase() || c == '_')
        || rest("local-command-", |c| c.is_ascii_lowercase())
        || rest("command-", |c| c.is_ascii_lowercase())
}

/// `text` without its noise blocks (`<system-reminder>…</system-reminder>` and the like). An opening tag with no
/// closing one is left as it is.
fn without_noise(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        let tail = &rest[open + 1..];
        let name_end = tail.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        let name = name_end.map_or("", |e| &tail[..e]);
        let closed = name_end
            .filter(|&e| tail[e..].starts_with('>') && noise_tag(name))
            .and_then(|e| {
                let body = &tail[e + 1..];
                body.find(&format!("</{name}>")).map(|c| &body[c + name.len() + 3..])
            });
        match closed {
            Some(after) => rest = after,
            None => {
                out.push('<');
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The text blocks of a line's message content, joined: a string is itself.
fn content_text(ev: &Value) -> String {
    match ev.pointer("/message/content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => (blocks.iter())
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .map(|b| b.get("text").and_then(Value::as_str).unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn flag(ev: &Value, key: &str) -> bool {
    ev.get(key).and_then(Value::as_bool) == Some(true)
}

/// Whether a `user` line is a background task's notice Claude Code put in the conversation ("Monitor … ended"):
/// none of the person's words.
fn is_notice(ev: &Value) -> bool {
    ev.get("type").and_then(Value::as_str) == Some("user")
        && !flag(ev, "isSidechain")
        && (ev.pointer("/origin/kind").and_then(Value::as_str) == Some("task-notification")
            || content_text(ev).trim_start().starts_with("<task-notification>"))
}

/// The person's own words in a `user` line, or "" when it's a tool's result, a subagent's or Claude Code's own.
pub fn person_text(ev: &Value) -> String {
    if ev.get("type").and_then(Value::as_str) != Some("user")
        || flag(ev, "isSidechain")
        || flag(ev, "isMeta")
        || flag(ev, "isCompactSummary")
        || is_notice(ev)
    {
        return String::new();
    }
    without_noise(&content_text(ev)).trim().to_string()
}

fn read_part(file: &Path, from: u64, bytes: u64) -> std::io::Result<String> {
    let mut f = File::open(file)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::new();
    f.take(bytes).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// The JSON lines of `text`; a line cut at either end of what was read, or not JSON, is left out.
pub(super) fn lines(text: &str) -> Vec<Value> {
    text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
}

/// Whether `path` is `dir` or inside it. Case is ignored on Windows, where paths are.
fn inside(path: &str, dir: &Path) -> bool {
    let fold = |s: String| if cfg!(windows) { s.to_lowercase() } else { s };
    Path::new(&fold(path.to_string())).starts_with(fold(dir.to_string_lossy().into_owned()))
}

/// The session's summary for the list, or `None` when it's none of the person's (no message of theirs, or in one of
/// `worktrees`, Retaskable's own lanes).
pub(super) fn describe(file: &Path, worktrees: &[PathBuf]) -> Option<ClaudeSession> {
    let meta = std::fs::metadata(file).ok()?;
    let head = lines(&read_part(file, 0, HEAD_BYTES).ok()?);
    let tail = if meta.len() > HEAD_BYTES {
        lines(&read_part(file, meta.len() - TAIL_BYTES.min(meta.len()), TAIL_BYTES).ok()?)
    } else {
        head.clone()
    };
    let first = head.iter().map(person_text).find(|t| !t.is_empty())?;
    let has_cwd = |e: &&Value| e.get("cwd").and_then(Value::as_str).is_some();
    let placed = tail.iter().rev().find(has_cwd).or_else(|| head.iter().find(has_cwd))?;
    let folder = placed.get("cwd").and_then(Value::as_str)?.to_string();
    if worktrees.iter().any(|w| inside(&folder, w)) {
        return None;
    }
    let ai_title = (tail.iter().rev())
        .find(|e| e.get("type").and_then(Value::as_str) == Some("ai-title"))
        .and_then(|e| e.get("aiTitle").and_then(Value::as_str))
        .filter(|t| !t.is_empty());
    let branch = (placed.get("gitBranch").and_then(Value::as_str))
        .filter(|b| !b.is_empty())
        .map(String::from);
    let at = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_millis() as u64;
    Some(ClaudeSession {
        id: file.file_stem()?.to_string_lossy().into_owned(),
        title: title(ai_title.unwrap_or(&first)),
        folder,
        branch,
        at,
        uncommitted: false,
    })
}

/// The session files under `root`, newest first.
fn files_newest_first(root: &Path) -> Vec<(PathBuf, std::time::SystemTime)> {
    let mut files = Vec::new();
    for dir in std::fs::read_dir(root).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            let path = f.path();
            if path.extension().is_some_and(|e| e == "jsonl")
                && let Ok(at) = f.metadata().and_then(|m| m.modified())
            {
                files.push((path, at));
            }
        }
    }
    files.sort_by_key(|(_, at)| std::cmp::Reverse(*at));
    files.truncate(LOOK_AT);
    files
}

/// `git status --short` in `folder`: "" when it's clean, not a checkout or gone.
pub fn status_of(folder: &Path) -> String {
    if !folder.is_dir() {
        return String::new();
    }
    crate::merge_update::run(&crate::git_bin::command, folder, &["status", "--short"]).unwrap_or_default()
}

/// The computer's recent Claude Code sessions under `root`, newest first, leaving out any in `worktrees`
/// (Retaskable's own lanes).
pub fn list(root: &Path, worktrees: &[PathBuf]) -> Vec<ClaudeSession> {
    let mut out: Vec<ClaudeSession> = files_newest_first(root)
        .iter()
        .filter_map(|(file, _)| describe(file, worktrees))
        .take(LIST)
        .collect();
    let mut dirty: std::collections::BTreeMap<String, bool> = Default::default();
    for s in &mut out {
        let folder = &s.folder;
        s.uncommitted = *dirty
            .entry(folder.clone())
            .or_insert_with(|| !status_of(Path::new(folder)).is_empty());
    }
    out
}

/// The session file with this id, in any of Claude's project folders. The id is checked before it is a file name.
pub fn session_file(root: &Path, id: &str) -> Option<PathBuf> {
    if !crate::backlog::expert::ExpertFrom::session_id_ok(id) {
        return None;
    }
    (std::fs::read_dir(root).ok()?.flatten())
        .map(|d| d.path().join(format!("{id}.jsonl")))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests;
