//! A native app's facts for the store lanes of the Release screen (docs/specs/small-agent-jobs.md, `nativeApp.ts`):
//! whether it builds for Android and iOS, its version, and what changed on the base branch since the last store
//! build. Read-only and plain code: files of a ref and `git`; the Expo config is read as text, never run.
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;
use serde_json::Value;

use crate::repo::git;

pub const MAX_PATHS: usize = 100;
pub const MAX_CHANGES: usize = 50;

/// A backlog item a commit can be matched to: by its merge commit, an `Item:` trailer, or the id a pull request
/// title starts with.
#[derive(Debug, Clone, Default)]
pub struct ItemRef {
    pub id: String,
    pub merge_commit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Change {
    pub sha: String,
    pub subject: String,
    #[serde(rename = "itemId", skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
}

/// What changed since the last store build.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChangedSince {
    pub sha: String,
    pub paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub more: Option<usize>,
    pub changes: Vec<Change>,
    #[serde(rename = "moreChanges", skip_serializing_if = "Option::is_none")]
    pub more_changes: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NativeApp {
    pub android: bool,
    pub ios: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(rename = "changedSince", skip_serializing_if = "Option::is_none")]
    pub changed_since: Option<ChangedSince>,
}

/// A file of `git_ref`, or `None`.
fn show(repo: &Path, git_ref: &str, file: &str) -> Option<String> {
    let out = git(repo).args(["show", &format!("{git_ref}:{file}")]).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_lines(repo: &Path, args: &[&str]) -> Option<Vec<String>> {
    let out = git(repo).args(args).output().ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    })
}

fn parse(text: Option<String>) -> Option<Value> {
    serde_json::from_str(&text?).ok()
}

/// Truthy like JavaScript's `!!`, for a config value.
fn present(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null | Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(_) => true,
    }
}

/// The Expo config's `(version, android, ios)`: `app.json`, else `app.config.js`.
fn expo_config(repo: &Path, git_ref: &str) -> (Option<String>, bool, bool) {
    if let Some(json) = parse(show(repo, git_ref, "app/app.json")) {
        let expo = json.get("expo").unwrap_or(&json);
        let version = expo.get("version").and_then(Value::as_str).map(str::to_string);
        return (version, present(expo.get("android")), present(expo.get("ios")));
    }
    static VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\bversion\s*:\s*["'`]([^"'`]+)["'`]"#).unwrap());
    static ANDROID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bandroid\s*:\s*\{").unwrap());
    static IOS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bios\s*:\s*\{").unwrap());
    let js = show(repo, git_ref, "app/app.config.js").unwrap_or_default();
    (
        VERSION.captures(&js).map(|c| c[1].to_string()),
        ANDROID.is_match(&js),
        IOS.is_match(&js),
    )
}

/// The sha of the newest `submitted` line in `testflight.log`; a torn line is skipped.
fn last_store_build(log_file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(log_file).ok()?;
    text.lines().rev().filter(|l| !l.is_empty()).find_map(|line| {
        let l: Value = serde_json::from_str(line).ok()?;
        (l["result"] == "submitted").then(|| l["sha"].as_str().map(str::to_string))?
    })
}

/// The commits touching `app/` since `sha`, oldest first, at most [`MAX_CHANGES`], and how many more.
fn change_list(repo: &Path, sha: &str, base: &str, items: &[ItemRef]) -> (Vec<Change>, Option<usize>) {
    static TITLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^([\w-]+?)(?:-piece-\d+)?:").unwrap());
    let range = format!("{sha}..{base}");
    let format = "--format=%H%x09%s%x09%(trailers:key=Item,valueonly,separator=%x2c)";
    let Some(lines) = git_lines(repo, &["log", "--reverse", format, &range, "--", "app/"]) else {
        return (Vec::new(), None);
    };
    let all: Vec<Change> = lines
        .iter()
        .map(|line| {
            let mut parts = line.split('\t');
            let hash = parts.next().unwrap_or_default();
            let subject = parts.next().unwrap_or_default();
            let trailer = parts.next().unwrap_or_default().trim();
            let title = TITLE.captures(subject).map(|c| c[1].to_string());
            let known = |id: &&str| !id.is_empty() && items.iter().any(|i| i.id == *id);
            let item_id = items
                .iter()
                .find(|i| i.merge_commit.as_deref() == Some(hash))
                .map(|i| i.id.clone())
                .or_else(|| {
                    [Some(trailer), title.as_deref()]
                        .into_iter()
                        .flatten()
                        .find(known)
                        .map(str::to_string)
                });
            Change {
                sha: hash.to_string(),
                subject: subject.to_string(),
                item_id,
            }
        })
        .collect();
    let more = all.len().checked_sub(MAX_CHANGES).filter(|n| *n > 0);
    (all.into_iter().take(MAX_CHANGES).collect(), more)
}

fn changed_paths(repo: &Path, sha: &str, base: &str, items: &[ItemRef]) -> Option<ChangedSince> {
    let paths = git_lines(repo, &["diff", "--name-only", sha, base])?;
    let more = paths.len().checked_sub(MAX_PATHS).filter(|n| *n > 0);
    let (changes, more_changes) = change_list(repo, sha, base, items);
    Some(ChangedSince {
        sha: sha.to_string(),
        paths: paths.into_iter().take(MAX_PATHS).collect(),
        more,
        changes,
        more_changes,
    })
}

/// The facts for the base branch `git_ref`, or `None` when it isn't an Expo app (no `app/eas.json`).
pub fn native_app(repo: &Path, git_ref: &str, log_file: &Path, items: &[ItemRef]) -> Option<NativeApp> {
    let eas = parse(show(repo, git_ref, "app/eas.json"))?;
    let (version, android, ios) = expo_config(repo, git_ref);
    let profiles: Vec<&Value> = eas
        .get("build")
        .and_then(Value::as_object)
        .map(|b| b.values().collect())
        .unwrap_or_default();
    let has = |os: &str, expo: bool| expo || profiles.iter().any(|p| p.is_object() && present(p.get(os)));
    let changed_since = last_store_build(log_file).and_then(|sha| changed_paths(repo, &sha, git_ref, items));
    Some(NativeApp {
        android: has("android", android),
        ios: has("ios", ios),
        version: version.filter(|v| !v.is_empty()),
        changed_since,
    })
}

#[cfg(test)]
mod tests;
