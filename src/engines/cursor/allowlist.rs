//! What a Cursor session may do: the fixed read/write/deny lists, checking an existing
//! `.cursor/cli.json` (or the person's own `~/.cursor/cli-config.json`) is sound, and writing the
//! lane's own allowlist and Claude isolate before every session (`prepareCursor`,
//! `shared/cursorAllow.ts` and `check.ts` in Retaskable.com).
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{Value, json};

use crate::engines::engine::SessionSpec;

use super::cursor_claude_config_dir;

const READ: [&str; 7] = [
    "Read(**)",
    "Shell(git)",
    "Shell(ls)",
    "Shell(cat)",
    "Shell(grep)",
    "Shell(rg)",
    "Shell(find)",
];
const WRITE: [&str; 1] = ["Write(**)"];
const DENY: [&str; 9] = [
    "Write(.git/**)",
    "Write(.cursor/**)",
    "Write(.retaskable/**)",
    "Read(.env*)",
    "Shell(rm)",
    "Shell(curl)",
    "Shell(wget)",
    "Shell(ssh)",
    "Shell(sudo)",
];

/// What an allow list grants that a session must not have ("allows Shell(*)", "allows writes
/// outside the workspace"), or none when it's fine.
fn grant_problem(file: &Value) -> Option<String> {
    let allow = allow_strings(file);
    if allow.iter().any(|e| e.starts_with("Shell(*")) {
        return Some("allows Shell(*)".to_string());
    }
    if allow.iter().any(|e| outside_write(e)) {
        return Some("allows writes outside the workspace".to_string());
    }
    None
}

fn allow_strings(file: &Value) -> Vec<&str> {
    file.pointer("/permissions/allow")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default()
}

/// A grant to write somewhere that isn't the workspace (`outsideWrite` in `shared/cursorAllow.ts`).
fn outside_write(entry: &str) -> bool {
    static WRITE_ENTRY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Write\((.+)\)$").unwrap());
    static ABS_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(/|\\|~|[A-Za-z]:)").unwrap());
    let Some(caps) = WRITE_ENTRY.captures(entry.trim()) else {
        return false;
    };
    let inner = caps.get(1).unwrap().as_str();
    if ABS_PATH.is_match(inner) {
        return true;
    }
    inner.split(['\\', '/']).any(|part| part == "..")
}

/// Why `file` can't be the allowlist a session runs under, or none when it's fine
/// (`allowlistProblem`).
fn allowlist_problem(file: &Value) -> Option<String> {
    if let Some(grant) = grant_problem(file) {
        return Some(format!("it {grant}"));
    }
    let deny: Vec<&str> = file
        .pointer("/permissions/deny")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let missing: Vec<&str> = DENY.iter().filter(|e| !deny.contains(e)).copied().collect();
    if !missing.is_empty() {
        return Some(format!("it lacks {}", missing.join(", ")));
    }
    None
}

/// The user's own `~/.cursor/cli-config.json` grants something a session must not have, or none
/// (`userAllowlistProblem`). No file at all is fine — only the lane's own allowlist is required.
fn user_allowlist_problem(file: &Path) -> Option<String> {
    if !file.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(file).unwrap_or_default();
    match serde_json::from_str::<Value>(&text) {
        Ok(parsed) => grant_problem(&parsed).map(|grant| format!("{} {grant}", file.display())),
        Err(_) => Some("it isn't valid json".to_string()),
    }
}

/// The fixed list with the code host's program (`gh`, `glab`, none for a project with no remote),
/// and for a writing role the project's own extra entries too (`allowlist`).
fn allowlist(readonly: bool, extra: &[String], cli: Option<&str>) -> Value {
    // No `version` key: cursor-agent rejects it in a project's .cursor/cli.json.
    let mut read: Vec<String> = vec![READ[0].to_string(), READ[1].to_string()];
    if let Some(cli) = cli {
        read.push(format!("Shell({cli})"));
    }
    read.extend(READ[2..].iter().map(|s| s.to_string()));

    let allow = if readonly {
        read
    } else {
        let mut combined = read;
        combined.extend(WRITE.iter().map(|s| s.to_string()));
        combined.extend(extra.iter().cloned());
        dedupe_owned(combined)
    };
    json!({ "permissions": { "allow": allow, "deny": DENY } })
}

fn dedupe_owned(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    items.into_iter().filter(|i| seen.insert(i.clone())).collect()
}

/// Why the repo's own tracked `.cursor/cli.json` in `cwd` would be refused, or none.
fn repo_allowlist_problem(cwd: &Path) -> Option<String> {
    let text = std::fs::read_to_string(cwd.join(".cursor").join("cli.json")).unwrap_or_default();
    match serde_json::from_str::<Value>(&text) {
        Ok(parsed) => allowlist_problem(&parsed),
        Err(_) => Some("isn't valid json".to_string()),
    }
}

fn git_cwd(cwd: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(cwd);
    cmd
}

/// True when the repo tracks `.cursor/cli.json`, so a session uses that file instead of ours.
fn tracked_allowlist(cwd: &Path) -> bool {
    git_cwd(cwd)
        .args(["ls-files", "--error-unmatch", ".cursor/cli.json"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Adds session-only Cursor files to the lane's git exclude, so a writing session can't commit
/// them.
fn exclude(cwd: &Path) -> Result<()> {
    let found = git_cwd(cwd)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()?;
    if !found.status.success() {
        return Ok(());
    }
    let rel = String::from_utf8_lossy(&found.stdout).trim().to_string();
    let file = cwd.join(rel);
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let mut have = std::fs::read_to_string(&file).unwrap_or_default();
    for line in [".cursor/cli.json", ".retaskable/cursor-isolate/"] {
        if have.lines().any(|l| l == line) {
            continue;
        }
        if !have.is_empty() && !have.ends_with('\n') {
            have.push('\n');
        }
        have.push_str(line);
        have.push('\n');
    }
    std::fs::write(&file, have).with_context(|| format!("writing {}", file.display()))
}

/// Claude settings with no hooks, so Cursor cannot import a Shell-rewriting `PreToolUse` from the
/// person (`prepareCursorIsolate`).
fn prepare_cursor_isolate(cwd: &Path) -> Result<()> {
    let dir = cursor_claude_config_dir(cwd);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let settings = json!({ "permissions": { "defaultMode": "dontAsk" } });
    let text = format!("{}\n", serde_json::to_string_pretty(&settings)?);
    std::fs::write(dir.join("settings.json"), text).context("writing the cursor isolate's claude settings")
}

/// `userAllowlistProblem`'s own default file: `~/.cursor/cli-config.json`, from the real home
/// directory (production use only — `user_allowlist_problem` itself stays pure and testable).
fn default_user_config() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
        .map(|home| PathBuf::from(home).join(".cursor").join("cli-config.json"))
}

/// Writes the role's allowlist into the lane and keeps it out of commits. A tracked
/// `.cursor/cli.json` is left as it is, as long as it's sound (`prepareCursor`).
pub(super) fn prepare_cursor(spec: &SessionSpec) -> Result<()> {
    if let Some(user_config) = default_user_config()
        && let Some(problem) = user_allowlist_problem(&user_config)
    {
        anyhow::bail!("Cursor session refused: {problem}");
    }
    let cwd = &spec.cwd;
    if tracked_allowlist(cwd) {
        if let Some(problem) = repo_allowlist_problem(cwd) {
            anyhow::bail!("Cursor session refused: the repo's .cursor/cli.json {problem}");
        }
        prepare_cursor_isolate(cwd)?;
        return exclude(cwd);
    }
    let dir = cwd.join(".cursor");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let cli: Option<&str> = match &spec.cursor {
        None => Some("gh"),
        Some(c) => c.cli.as_deref(),
    };
    let extra: &[String] = spec.cursor.as_ref().map(|c| c.allow.as_slice()).unwrap_or(&[]);
    let value = allowlist(spec.readonly, extra, cli);
    let text = format!("{}\n", serde_json::to_string_pretty(&value)?);
    std::fs::write(dir.join("cli.json"), text).context("writing .cursor/cli.json")?;
    prepare_cursor_isolate(cwd)?;
    exclude(cwd)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::super::{CursorEngine, base_spec, cursor_env};
    use super::*;
    use crate::engines::engine::Engine;

    fn init_repo(dir: &Path) {
        Command::new("git").arg("init").current_dir(dir).output().unwrap();
    }

    // allowlist_problem / grant_problem / outside_write

    #[test]
    fn allowlist_problem_flags_shell_star_outside_writes_and_a_short_deny_list() {
        let ok = json!({"permissions": {"allow": ["Read(**)", "Shell(git)", "Write(src/**)"], "deny": DENY}});
        assert_eq!(allowlist_problem(&ok), None);
        assert_eq!(
            allowlist_problem(&json!({"permissions": {"allow": ["Shell(*extra)"], "deny": DENY}})),
            Some("it allows Shell(*)".to_string())
        );
        for entry in ["Write(~/x)", "Write(../x)", "Write(/tmp/**)"] {
            assert_eq!(
                allowlist_problem(&json!({"permissions": {"allow": [entry], "deny": DENY}})),
                Some("it allows writes outside the workspace".to_string()),
                "{entry}"
            );
        }
        let short_deny: Vec<&str> = DENY.iter().filter(|e| **e != "Shell(sudo)").copied().collect();
        assert_eq!(
            allowlist_problem(&json!({"permissions": {"allow": ["Read(**)"], "deny": short_deny}})),
            Some("it lacks Shell(sudo)".to_string())
        );
        assert!(allowlist_problem(&json!({})).unwrap().starts_with("it lacks "));
    }

    // allowlist

    #[test]
    fn allowlist_adds_the_code_hosts_program_or_none() {
        assert!(
            allowlist(true, &[], Some("gh"))["permissions"]["allow"]
                .as_array()
                .unwrap()
                .contains(&json!("Shell(gh)"))
        );
        let glab = allowlist(false, &[], Some("glab"));
        let allow = glab["permissions"]["allow"].as_array().unwrap();
        assert!(allow.contains(&json!("Shell(glab)")));
        assert!(!allow.contains(&json!("Shell(gh)")));
        assert!(
            allowlist(true, &[], Some("glab"))["permissions"]["allow"]
                .as_array()
                .unwrap()
                .contains(&json!("Shell(glab)"))
        );
        let none = allowlist(false, &[], None);
        let allow = none["permissions"]["allow"].as_array().unwrap();
        assert!(!allow.iter().any(|e| e == "Shell(gh)" || e == "Shell(glab)"));
    }

    #[test]
    fn allowlist_adds_extra_entries_only_for_a_writing_role() {
        let extra = vec!["Shell(npm)".to_string(), "Shell(make)".to_string()];
        let writing = allowlist(false, &extra, Some("gh"));
        let allow = writing["permissions"]["allow"].as_array().unwrap();
        assert!(allow.contains(&json!("Shell(npm)")));
        assert!(allow.contains(&json!("Write(**)")));
        let read_only = allowlist(true, &extra, Some("gh"));
        let allow = read_only["permissions"]["allow"].as_array().unwrap();
        assert!(!allow.contains(&json!("Shell(npm)")));
        assert!(!allow.contains(&json!("Write(**)")));
    }

    // user_allowlist_problem

    #[test]
    fn user_allowlist_problem_is_none_for_a_missing_file_and_names_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(user_allowlist_problem(&dir.path().join("missing.json")), None);

        let file = dir.path().join("cli-config.json");
        std::fs::write(&file, "{").unwrap();
        assert_eq!(user_allowlist_problem(&file), Some("it isn't valid json".to_string()));

        std::fs::write(&file, json!({"permissions": {"allow": ["Shell(*)"]}}).to_string()).unwrap();
        assert_eq!(
            user_allowlist_problem(&file),
            Some(format!("{} allows Shell(*)", file.display()))
        );

        std::fs::write(
            &file,
            json!({"permissions": {"allow": ["Write(~/secrets)"]}}).to_string(),
        )
        .unwrap();
        assert_eq!(
            user_allowlist_problem(&file),
            Some(format!("{} allows writes outside the workspace", file.display()))
        );

        std::fs::write(
            &file,
            json!({"permissions": {"allow": ["Shell(git)", "Write(**)"]}}).to_string(),
        )
        .unwrap();
        assert_eq!(
            user_allowlist_problem(&file),
            None,
            "no deny list isn't itself a refusal"
        );
    }

    // prepare_cursor (through the Engine trait, so it exercises the real home-config path too)

    #[test]
    fn prepare_cursor_writes_the_allowlist_isolate_and_exclude_for_an_untracked_lane() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let mut spec = base_spec(dir.path());
        spec.readonly = false;
        CursorEngine.prepare(&spec).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.path().join(".cursor").join("cli.json")).unwrap())
                .unwrap();
        let allow = written["permissions"]["allow"].as_array().unwrap();
        assert!(allow.contains(&json!("Read(**)")));
        assert!(allow.contains(&json!("Write(**)")));
        assert!(
            written["permissions"]["deny"]
                .as_array()
                .unwrap()
                .contains(&json!("Shell(rm)"))
        );

        let claude_dir = cursor_claude_config_dir(dir.path());
        let claude: Value =
            serde_json::from_str(&std::fs::read_to_string(claude_dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(claude.get("hooks"), None, "no PreToolUse hooks in the isolate");
        assert_eq!(
            cursor_env(&spec),
            vec![(
                "CLAUDE_CONFIG_DIR".to_string(),
                claude_dir.to_string_lossy().into_owned()
            )]
        );

        let exclude_rel = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", "--git-path", "info/exclude"])
                .current_dir(dir.path())
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        let excluded = std::fs::read_to_string(dir.path().join(exclude_rel.trim())).unwrap();
        assert!(excluded.contains(".cursor/cli.json"));
        assert!(excluded.contains(".retaskable/cursor-isolate/"));
    }

    #[test]
    fn prepare_cursor_leaves_a_sound_tracked_allowlist_alone_and_refuses_a_short_one() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let cursor_dir = dir.path().join(".cursor");
        std::fs::create_dir_all(&cursor_dir).unwrap();
        let own = json!({"permissions": {"allow": ["Read(**)", "Shell(git)"], "deny": DENY}});
        let cli_json = cursor_dir.join("cli.json");
        std::fs::write(&cli_json, own.to_string()).unwrap();
        Command::new("git")
            .args(["add", ".cursor/cli.json"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-c",
                "user.email=t@example.com",
                "-c",
                "user.name=t",
                "commit",
                "-m",
                "allow",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();

        let spec = base_spec(dir.path());
        CursorEngine.prepare(&spec).unwrap();
        assert_eq!(
            std::fs::read_to_string(&cli_json).unwrap(),
            own.to_string(),
            "left alone"
        );

        let short = json!({"permissions": {"allow": ["Read(**)"], "deny": ["Shell(rm)"]}});
        std::fs::write(&cli_json, short.to_string()).unwrap();
        let err = CursorEngine.prepare(&spec).unwrap_err();
        assert!(
            err.to_string()
                .contains("Cursor session refused: the repo's .cursor/cli.json it lacks "),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(&cli_json).unwrap(),
            short.to_string(),
            "a refusal rewrites nothing"
        );
    }
}
