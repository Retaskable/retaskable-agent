//! The Cursor engine: `cursor-agent -p` with stream-json out. The result is the last fenced
//! `json` block (or the last top-level object) of the final message, checked against the same
//! schema subset Claude's `--json-schema` enforces (docs/specs/engines/02-the-engine-layer.md
//! sections 4 to 6 and 8). Never `--force`: what a session may do comes from the allowlist
//! written beside it (`.cursor/cli.json`), not from a flag. Following `agent/src/engines/cursor.ts`,
//! `jsonResult.ts`, `check.ts` and `shared/cursorAllow.ts`/`shared/text.ts` in Retaskable.com (the
//! ground truth this is porting).
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;
use serde_json::{Value, json};

use super::engine::{
    Engine, EngineId, EngineReader, EventKind, SessionEvent, SessionResult, SessionSpec, SessionStart, Tokens,
};

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

const TOOLS: [(&str, &str); 6] = [
    ("shellToolCall", "Bash"),
    ("readToolCall", "Read"),
    ("editToolCall", "Edit"),
    ("writeToolCall", "Write"),
    ("grepToolCall", "Grep"),
    ("globToolCall", "Glob"),
];

/// Why a completed Shell tool call was refused, or none when it ran (`shellDenial`, engines.md
/// section 8). Cursor's allowlist refusals often carry only an empty `reason`.
const EMPTY_DENIAL: &str =
    "not on the allowlist (Shell matches the command's first word only; use a bare allowed program, never a wrapper)";

/// `cursorArgs(spec)`: print mode, `stream-json` out, trusted (never asked to approve), the
/// chosen model (or "auto"), `--mode ask` for a read-only role, and `--resume` for a session
/// that already has an id. Never `--force`/`--yolo` — what a session may do is the allowlist's.
fn cursor_args(spec: &SessionSpec) -> Vec<String> {
    let model = spec
        .cursor
        .as_ref()
        .and_then(|c| c.model.as_deref())
        .filter(|m| !m.is_empty())
        .unwrap_or("auto");
    let mut args = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--trust".to_string(),
        "--model".to_string(),
        model.to_string(),
    ];
    if spec.readonly {
        args.push("--mode".to_string());
        args.push("ask".to_string());
    }
    match &spec.start {
        SessionStart::Ephemeral => {}
        SessionStart::Fresh(id) | SessionStart::Resume(id) => {
            args.push("--resume".to_string());
            args.push(id.clone());
        }
    }
    args
}

static SHELL_ENTRY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^Shell\((.+)\)$").unwrap());

/// `shellPrograms(spec)`: what a writing Cursor session may run as Shell's first word, for the
/// prompt (engines.md section 6). A read-only session is never told about its own extra entries,
/// since it can't use them.
fn shell_programs(spec: &SessionSpec) -> String {
    let from_allow: Vec<&str> = spec
        .cursor
        .as_ref()
        .map(|c| c.allow.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter_map(|e| SHELL_ENTRY.captures(e))
        .map(|c| c.get(1).unwrap().as_str())
        .filter(|p| !p.is_empty() && *p != "*")
        .collect();
    let cli = spec.cursor.as_ref().and_then(|c| c.cli.as_deref());

    let mut base: Vec<&str> = vec!["git", "ls", "cat", "grep", "rg", "find"];
    if let Some(cli) = cli {
        base.push(cli);
    }
    if !spec.readonly {
        base.extend(from_allow);
    }
    dedupe_refs(base).join(", ")
}

/// `cursorPrompt(spec)`: the prompt plus the result instructions. Cursor has no `--json-schema`,
/// so the schema is spelled out and the result must be the final message's own fenced block.
fn cursor_prompt(spec: &SessionSpec) -> String {
    let shells = shell_programs(spec);
    format!(
        "{}\n\n\
         Shell commands must start with an allowed program ({shells}). Match is the first word only: type `git status`, \
         never a personal wrapper, pager or shell launcher in front. If a Shell call is refused with an empty reason, \
         the first word was not on that list — retry the bare program, do not try other wrappers.\n\n\
         When you've finished, end your final message with exactly one fenced ```json block holding your result, \
         matching this schema. Nothing after it.\n\n\
         {}",
        spec.prompt, spec.schema,
    )
}

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

fn dedupe_refs(items: Vec<&str>) -> Vec<&str> {
    let mut seen = HashSet::new();
    items.into_iter().filter(|i| seen.insert(*i)).collect()
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

/// Session-local Claude config under the lane (engines.md sections 4 and 6.1): Cursor loads the
/// person's `~/.claude` `PreToolUse` hooks (which can rewrite a Shell call so the allowlist never
/// sees it) unless `CLAUDE_CONFIG_DIR` points at a folder without those hooks.
fn cursor_claude_config_dir(cwd: &Path) -> PathBuf {
    cwd.join(".retaskable").join("cursor-isolate").join("claude")
}

/// Env so a Cursor lane does not load the person's interactive Claude hooks (`cursorEnv`).
fn cursor_env(spec: &SessionSpec) -> Vec<(String, String)> {
    vec![(
        "CLAUDE_CONFIG_DIR".to_string(),
        cursor_claude_config_dir(&spec.cwd).to_string_lossy().into_owned(),
    )]
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
fn prepare_cursor(spec: &SessionSpec) -> Result<()> {
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

static FENCE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)```json\s*(.*?)```").unwrap());

/// The last ` ```json ` block, or the last top-level `{…}` that parses. `None` when there's
/// neither (`extractJson`).
fn extract_json(text: &str) -> Option<Value> {
    let fences: Vec<&str> = FENCE.captures_iter(text).map(|c| c.get(1).unwrap().as_str()).collect();
    for fence in fences.iter().rev() {
        if let Ok(v) = serde_json::from_str(fence) {
            return Some(v);
        }
    }
    let objects = top_level_objects(text);
    for object in objects.iter().rev() {
        if let Ok(v) = serde_json::from_str(object) {
            return Some(v);
        }
    }
    None
}

/// Balanced `{…}` spans that start outside any other object (`topLevelObjects`).
fn top_level_objects(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0u32;
    let mut start = None;
    let mut quote = false;
    let mut escape = false;
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if quote {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                quote = false;
            }
            continue;
        }
        if c == '"' {
            quote = true;
        } else if c == '{' {
            if depth == 0 {
                start = Some(i);
            }
            depth += 1;
        } else if c == '}' && depth > 0 {
            depth -= 1;
            if depth == 0
                && let Some(s) = start
            {
                out.push(chars[s..=i].iter().collect());
            }
        }
    }
    out
}

/// Short lines, like "`status`: must be one of pr_opened, needs_clarification, failed"
/// (`checkSchema`): only the keywords a `--json-schema` result schema uses.
fn check_schema(schema: &Value, value: &Value, at: &str) -> Vec<String> {
    if !schema.is_object() {
        return Vec::new();
    }
    let where_at = if at.is_empty() { "result" } else { at };
    if let Some(enum_values) = schema.get("enum").and_then(Value::as_array)
        && !enum_values.contains(value)
    {
        let names: Vec<String> = enum_values.iter().map(display_value).collect();
        return vec![format!("{where_at}: must be one of {}", names.join(", "))];
    }
    match schema.get("type").and_then(Value::as_str) {
        Some("object") => {
            let Some(obj) = value.as_object() else {
                return vec![format!("{where_at}: must be an object")];
            };
            let mut errors = Vec::new();
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for key in required.iter().filter_map(Value::as_str) {
                    if !obj.contains_key(key) {
                        let key_at = if at.is_empty() {
                            key.to_string()
                        } else {
                            format!("{at}.{key}")
                        };
                        errors.push(format!("{key_at}: is required"));
                    }
                }
            }
            let no_props = json!({});
            let props = schema.get("properties").unwrap_or(&no_props);
            for (key, child) in obj {
                let child_at = if at.is_empty() {
                    key.clone()
                } else {
                    format!("{at}.{key}")
                };
                if let Some(child_schema) = props.get(key) {
                    errors.extend(check_schema(child_schema, child, &child_at));
                } else if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                    errors.push(format!("{child_at}: is not allowed"));
                }
            }
            errors
        }
        Some("array") => {
            let Some(items) = value.as_array() else {
                return vec![format!("{where_at}: must be an array")];
            };
            match schema.get("items") {
                Some(item_schema) => items
                    .iter()
                    .enumerate()
                    .flat_map(|(i, item)| check_schema(item_schema, item, &format!("{where_at}[{i}]")))
                    .collect(),
                None => Vec::new(),
            }
        }
        Some("string") if !value.is_string() => vec![format!("{where_at}: must be a string")],
        Some("number") if !value.is_number() => vec![format!("{where_at}: must be a number")],
        Some("boolean") if !value.is_boolean() => vec![format!("{where_at}: must be a boolean")],
        _ => Vec::new(),
    }
}

fn display_value(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// `s`, cut to at most `n` chars, with a trailing `…` when it was longer (`clip` in
/// `shared/text.ts`). Duplicated from `summarize.rs`, which stays Claude-only.
fn clip(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    let mut out: String = chars[..n.saturating_sub(1)].iter().collect();
    out.push('…');
    out
}

/// The last (up to) `max_chars` characters of `s`, on a char boundary (`tail` in
/// `shared/text.ts`).
fn tail(s: &str, max_chars: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max_chars {
        s.to_string()
    } else {
        chars[chars.len() - max_chars..].iter().collect()
    }
}

/// The tool name plus its first matching argument, for a transcript drawer (`toolText`).
fn tool_text(ev: &Value) -> String {
    let no_call = json!({});
    let call = ev.get("tool_call").unwrap_or(&no_call);
    let key = call
        .as_object()
        .and_then(|obj| obj.keys().find(|k| k.ends_with("ToolCall")));
    let no_args = json!({});
    let args = key
        .and_then(|k| call.get(k))
        .and_then(|v| v.get("args"))
        .or_else(|| ev.get("args"))
        .unwrap_or(&no_args);
    let name = match key {
        Some(k) => TOOLS
            .iter()
            .find(|(t, _)| t == k)
            .map(|(_, n)| n.to_string())
            .unwrap_or_else(|| k.trim_end_matches("ToolCall").to_string()),
        None => ev.get("name").and_then(Value::as_str).unwrap_or("tool").to_string(),
    };
    let arg = ["command", "path", "file_path", "pattern", "glob_pattern"]
        .iter()
        .find_map(|k| args.get(k).and_then(Value::as_str))
        .unwrap_or("");
    if arg.is_empty() {
        name
    } else {
        let collapsed = arg.split_whitespace().collect::<Vec<_>>().join(" ");
        format!("{name}: {}", clip(&collapsed, 160))
    }
}

/// Why a completed Shell tool call was refused, or `None` when it ran (`shellDenial`). An empty
/// engine reason becomes a concrete allowlist hint.
fn shell_denial(ev: &Value) -> Option<String> {
    let no_call = json!({});
    let call = ev.get("tool_call").unwrap_or(&no_call);
    let shell = call.get("shellToolCall").or_else(|| call.get("ShellToolCall"))?;
    let no_result = json!({});
    let result = shell.get("result").unwrap_or(&no_result);
    let reason = result
        .get("reason")
        .and_then(Value::as_str)
        .or_else(|| result.get("message").and_then(Value::as_str))
        .or_else(|| result.get("error").and_then(Value::as_str))
        .or_else(|| ev.get("reason").and_then(Value::as_str))
        .unwrap_or("")
        .trim();
    let bool_field = |key: &str| result.get(key).and_then(Value::as_bool) == Some(true);
    let str_field_is = |key: &str, want: &str| result.get(key).and_then(Value::as_str) == Some(want);
    let missing_or_null = |key: &str| result.get(key).is_none_or(Value::is_null);
    let denied = bool_field("rejected")
        || bool_field("denied")
        || str_field_is("permission", "deny")
        || str_field_is("permission", "denied")
        || str_field_is("status", "rejected")
        || str_field_is("status", "denied")
        || ev.get("subtype").and_then(Value::as_str) == Some("error")
        || (result.get("reason").is_some()
            && missing_or_null("exitCode")
            && missing_or_null("stdout")
            && missing_or_null("output"));
    if !denied {
        return None;
    }
    Some(if reason.is_empty() {
        EMPTY_DENIAL.to_string()
    } else {
        reason.to_string()
    })
}

/// `summarizeCursor(ev, at)`: an `assistant` event's text, a started/refused `tool_call`, and a
/// `result` event become the same `SessionEvent` shape Claude's reader produces.
fn summarize_cursor(event: &Value, at: i64) -> Vec<SessionEvent> {
    match event.get("type").and_then(Value::as_str) {
        Some("assistant") => {
            let text = event
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|c| c.get("text").and_then(Value::as_str))
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![SessionEvent {
                    at,
                    kind: EventKind::Text,
                    text: clip(&text, 800),
                }]
            }
        }
        Some("tool_call") if event.get("subtype").and_then(Value::as_str) == Some("started") => {
            vec![SessionEvent {
                at,
                kind: EventKind::Tool,
                text: tool_text(event),
            }]
        }
        Some("tool_call")
            if matches!(
                event.get("subtype").and_then(Value::as_str),
                Some("completed") | Some("error")
            ) =>
        {
            shell_denial(event)
                .map(|why| {
                    vec![SessionEvent {
                        at,
                        kind: EventKind::Tool,
                        text: format!("Bash refused: {}", clip(&why, 200)),
                    }]
                })
                .unwrap_or_default()
        }
        Some("result") => {
            let done = event.get("subtype").and_then(Value::as_str) == Some("success")
                && event.get("is_error").and_then(Value::as_bool) != Some(true);
            let text = if done {
                "done".to_string()
            } else {
                event.get("subtype").and_then(Value::as_str).unwrap_or("").to_string()
            };
            vec![SessionEvent {
                at,
                kind: EventKind::Result,
                text: format!("Finished: {text}"),
            }]
        }
        _ => Vec::new(),
    }
}

static LIMIT_TEXT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)usage limit|rate limit").unwrap());

/// Whether a result event's own words say the usage limit was hit (`limitText`).
fn limit_text(ev: &Value) -> bool {
    let result = ev.get("result").and_then(Value::as_str).unwrap_or("");
    let error = ev.get("error").and_then(Value::as_str).unwrap_or("");
    LIMIT_TEXT.is_match(&format!("{result} {error}"))
}

/// A result event's `usage` as the session's tokens, or `None` when it carries no counts
/// (`tokensOf`). Cache counts are left out — they're not part of what the session was billed for.
fn tokens_of(usage: Option<&Value>) -> Option<Tokens> {
    let n = |key: &str| -> u64 {
        usage
            .and_then(|u| u.get(key))
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && *v > 0.0)
            .map(|v| v as u64)
            .unwrap_or(0)
    };
    let input = n("inputTokens");
    let output = n("outputTokens");
    if input > 0 || output > 0 {
        Some(Tokens { input, output })
    } else {
        None
    }
}

fn result_or(ev: &Value, default: &str) -> String {
    ev.get("result")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(default)
        .to_string()
}

/// One session's reader: the final message's own fenced `json` block (or last top-level object)
/// becomes the structured result, checked against `schema` (`cursor.reader(spec, sessionId)`).
pub struct CursorReader {
    schema: Value,
    last_text: String,
    /// The latest `session_id` seen on any event, so a crash before a `result` event still
    /// reports the chat Cursor actually opened.
    chat: String,
}

impl CursorReader {
    fn new(schema: Value) -> Self {
        Self {
            schema,
            last_text: String::new(),
            chat: String::new(),
        }
    }

    fn session_id(&self, known_session_id: &str) -> String {
        if self.chat.is_empty() {
            known_session_id.to_string()
        } else {
            self.chat.clone()
        }
    }
}

impl EngineReader for CursorReader {
    fn line(&mut self, line: &str, known_session_id: &str) -> Option<SessionResult> {
        let event: Value = serde_json::from_str(line).ok()?;
        if let Some(session_id) = event.get("session_id").and_then(Value::as_str) {
            self.chat = session_id.to_string();
        }
        if event.get("type").and_then(Value::as_str) == Some("assistant") {
            let text = event
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(Value::as_array)
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|c| c.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            if !text.trim().is_empty() {
                self.last_text = text;
            }
        }
        if event.get("type").and_then(Value::as_str) != Some("result") {
            return None;
        }

        let is_error = event.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        let raw_subtype = event.get("subtype").and_then(Value::as_str).unwrap_or("").to_string();
        let raw = event
            .get("result")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| self.last_text.clone());
        let structured = if is_error { None } else { extract_json(&raw) };
        let errors = match &structured {
            Some(v) => check_schema(&self.schema, v, ""),
            None => Vec::new(),
        };

        let api_429 = event.get("api_error_status").and_then(Value::as_u64) == Some(429);
        let limit_hit = api_429 || (is_error && limit_text(&event));

        let ok = raw_subtype == "success" && !is_error && structured.is_some() && errors.is_empty();
        let subtype = if limit_hit || is_error {
            "error".to_string()
        } else if !errors.is_empty() || structured.is_none() {
            "invalid_result".to_string()
        } else {
            raw_subtype.clone()
        };
        let error = if limit_hit {
            Some(result_or(&event, "usage limit"))
        } else if is_error {
            Some(result_or(&event, &raw_subtype))
        } else if !errors.is_empty() {
            Some(errors.join("; "))
        } else if structured.is_none() {
            Some("No structured output returned".to_string())
        } else {
            None
        };

        Some(SessionResult {
            ok,
            subtype,
            structured: if errors.is_empty() { structured } else { None },
            error,
            cost_usd: None,
            session_id: self.session_id(known_session_id),
            limit_hit,
            signed_out: false,
            context_tokens: None,
            usage: None,
            tokens: tokens_of(event.get("usage")),
        })
    }

    fn no_result(&self, exit_code: Option<i32>, stderr: &str, known_session_id: &str) -> SessionResult {
        let code = exit_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let error = format!(
            "cursor-agent exited with code {code} and no result. {}",
            tail(stderr.trim(), 1500)
        );
        SessionResult {
            ok: false,
            subtype: "exited".to_string(),
            structured: None,
            error: Some(error),
            cost_usd: None,
            session_id: self.session_id(known_session_id),
            limit_hit: LIMIT_TEXT.is_match(stderr),
            signed_out: false,
            context_tokens: None,
            usage: None,
            tokens: None,
        }
    }
}

/// The Cursor engine: `cursorArgs`, `cursorEnv`, `cursorPrompt`, `prepareCursor` and the
/// `stream-json` reader above, behind the `Engine` interface the runner calls through.
pub struct CursorEngine;

impl Engine for CursorEngine {
    fn id(&self) -> EngineId {
        EngineId::Cursor
    }

    fn binary(&self, env: &dyn Fn(&str) -> Option<String>, windows: bool, path_dirs: &[PathBuf]) -> Result<PathBuf> {
        let home = env("HOME").or_else(|| env("USERPROFILE")).map(PathBuf::from);
        Ok(crate::setup_check::resolve_cursor(env, windows, path_dirs, home))
    }

    fn args(&self, spec: &SessionSpec) -> Vec<String> {
        cursor_args(spec)
    }

    fn env(&self, spec: &SessionSpec) -> Vec<(String, String)> {
        cursor_env(spec)
    }

    fn prompt(&self, spec: &SessionSpec) -> String {
        cursor_prompt(spec)
    }

    fn prepare(&self, spec: &SessionSpec) -> Result<()> {
        prepare_cursor(spec)
    }

    fn reader(&self, spec: &SessionSpec) -> Box<dyn EngineReader> {
        Box::new(CursorReader::new(spec.schema.clone()))
    }

    fn summarize(&self, event: &Value, at: i64) -> Vec<SessionEvent> {
        summarize_cursor(event, at)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::engines::engine::CursorOptions;

    fn base_spec(cwd: &Path) -> SessionSpec {
        SessionSpec {
            engine: Some(EngineId::Cursor),
            model: "sonnet".to_string(),
            prompt: "plan the board".to_string(),
            schema: json!({"type": "object"}),
            cwd: cwd.to_path_buf(),
            max_turns: None,
            budget_usd: 0.0,
            start: SessionStart::Ephemeral,
            readonly: true,
            cursor: None,
        }
    }

    fn init_repo(dir: &Path) {
        Command::new("git").arg("init").current_dir(dir).output().unwrap();
    }

    // cursor_args / shell_programs / cursor_prompt

    #[test]
    fn cursor_args_prints_streams_asks_when_read_only_and_never_forces() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = base_spec(dir.path());
        spec.start = SessionStart::Fresh("chat-1".to_string());
        let args = cursor_args(&spec);
        assert_eq!(
            args,
            vec![
                "-p",
                "--output-format",
                "stream-json",
                "--trust",
                "--model",
                "auto",
                "--mode",
                "ask",
                "--resume",
                "chat-1"
            ]
        );
        assert!(!args.contains(&"--force".to_string()));
        assert!(!args.contains(&"--yolo".to_string()));

        let mut writing = base_spec(dir.path());
        writing.readonly = false;
        writing.start = SessionStart::Resume("chat-2".to_string());
        assert!(!cursor_args(&writing).contains(&"--mode".to_string()));
    }

    #[test]
    fn cursor_args_uses_the_configured_model_and_an_ephemeral_start_has_no_resume() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = base_spec(dir.path());
        spec.cursor = Some(CursorOptions {
            model: Some("gpt-5".to_string()),
            ..Default::default()
        });
        let args = cursor_args(&spec);
        assert_eq!(args[args.iter().position(|a| a == "--model").unwrap() + 1], "gpt-5");
        assert!(!args.contains(&"--resume".to_string()));
    }

    #[test]
    fn cursor_prompt_puts_the_schema_at_the_end_with_bare_shell_programs() {
        let dir = tempfile::tempdir().unwrap();
        let spec = base_spec(dir.path());
        let text = cursor_prompt(&spec);
        assert!(text.contains("```json block"));
        assert!(text.contains("Shell commands must start with an allowed program"));
        assert!(text.contains("git"));
        assert!(text.contains("never a personal wrapper"));
        assert!(text.ends_with(&spec.schema.to_string()));

        let mut writing = base_spec(dir.path());
        writing.readonly = false;
        writing.cursor = Some(CursorOptions {
            allow: vec!["Shell(npm)".to_string()],
            cli: Some("gh".to_string()),
            ..Default::default()
        });
        assert!(cursor_prompt(&writing).contains("npm"));
    }

    // extract_json

    #[test]
    fn extract_json_takes_the_last_fenced_block_then_the_last_top_level_object_then_none() {
        assert_eq!(
            extract_json("notes\n```json\n{\"ok\":false}\n```\nthen\n```json\n{\"ok\":true}\n```\nthanks"),
            Some(json!({"ok": true}))
        );
        assert_eq!(
            extract_json("no fence, just {\"ok\":true} at the end"),
            Some(json!({"ok": true}))
        );
        assert_eq!(extract_json("nothing here"), None);
    }

    // check_schema

    #[test]
    fn check_schema_names_an_enum_miss_and_a_required_miss() {
        let schema = json!({
            "type": "object",
            "properties": {"status": {"enum": ["pr_opened", "failed"]}},
            "required": ["status"],
        });
        assert_eq!(
            check_schema(&schema, &json!({"status": "pr_opened"}), ""),
            Vec::<String>::new()
        );
        assert_eq!(
            check_schema(&schema, &json!({"status": "nope"}), ""),
            vec!["status: must be one of pr_opened, failed".to_string()]
        );
        assert_eq!(
            check_schema(&schema, &json!({}), ""),
            vec!["status: is required".to_string()]
        );
    }

    #[test]
    fn check_schema_names_a_type_mismatch_and_disallows_extras_when_asked() {
        let schema = json!({
            "type": "object",
            "properties": {"ok": {"type": "boolean"}},
            "required": ["ok"],
            "additionalProperties": false,
        });
        let errors = check_schema(&schema, &json!({"ok": "yes"}), "");
        assert!(errors[0].contains("ok: must be a boolean"), "{errors:?}");
        let errors = check_schema(&schema, &json!({"ok": true, "extra": 1}), "");
        assert!(errors.contains(&"extra: is not allowed".to_string()));
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

    // shell_denial / tool_text

    #[test]
    fn shell_denial_is_none_when_it_ran_and_reports_the_reason_or_the_empty_hint() {
        assert_eq!(
            shell_denial(
                &json!({"type": "tool_call", "subtype": "completed", "tool_call": {"shellToolCall": {"result": {}}}})
            ),
            None
        );
        let denied = shell_denial(&json!({
            "type": "tool_call", "subtype": "completed",
            "tool_call": {"shellToolCall": {"result": {"rejected": true, "reason": ""}}},
        }))
        .unwrap();
        assert!(denied.contains("not on the allowlist"), "{denied}");
        assert_eq!(
            shell_denial(&json!({
                "type": "tool_call", "subtype": "completed",
                "tool_call": {"shellToolCall": {"result": {"rejected": true, "reason": "Shell(rtk) denied"}}},
            })),
            Some("Shell(rtk) denied".to_string())
        );
    }

    #[test]
    fn tool_text_names_the_known_tools_by_their_claude_names() {
        let ev = json!({"tool_call": {"shellToolCall": {"args": {"command": "git status"}}}});
        assert_eq!(tool_text(&ev), "Bash: git status");
    }

    // summarize_cursor

    #[test]
    fn summarize_cursor_reports_assistant_text_a_started_tool_a_refusal_and_the_result() {
        assert_eq!(
            summarize_cursor(&json!({"type": "system", "subtype": "init", "session_id": "chat-1"}), 1),
            Vec::new()
        );
        let started = summarize_cursor(
            &json!({"type": "tool_call", "subtype": "started", "tool_call": {"shellToolCall": {"args": {"command": "git status"}}}}),
            1,
        );
        assert_eq!(started[0].text, "Bash: git status");

        assert_eq!(
            summarize_cursor(
                &json!({"type": "tool_call", "subtype": "completed", "tool_call": {"shellToolCall": {"result": {}}}}),
                1
            ),
            Vec::new()
        );
        let refused = summarize_cursor(
            &json!({"type": "tool_call", "subtype": "completed", "tool_call": {"shellToolCall": {"result": {"reason": ""}}}}),
            1,
        );
        assert!(
            refused[0].text.starts_with("Bash refused: not on the allowlist"),
            "{:?}",
            refused[0]
        );

        assert_eq!(
            summarize_cursor(&json!({"type": "result", "subtype": "success"}), 9)[0].text,
            "Finished: done"
        );
    }

    // CursorReader::line / no_result

    fn schema() -> Value {
        json!({"type": "object", "properties": {"ok": {"type": "boolean"}}, "required": ["ok"]})
    }

    #[test]
    fn reader_returns_the_structured_result_from_the_final_fenced_block() {
        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"success","is_error":false,"result":"```json\n{\"ok\":true}\n```\nall done","session_id":"chat-1"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(result.ok);
        assert_eq!(result.structured, Some(json!({"ok": true})));
        assert_eq!(result.cost_usd, None);
        assert_eq!(result.session_id, "chat-1");
    }

    #[test]
    fn reader_names_a_schema_miss_as_invalid_result() {
        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"success","result":"```json\n{\"ok\":\"yes\"}\n```"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(!result.ok);
        assert_eq!(result.subtype, "invalid_result");
        assert!(result.error.unwrap().contains("ok: must be a boolean"));
    }

    #[test]
    fn reader_reports_no_structured_output_when_the_final_message_has_none() {
        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"success","result":"I forgot"}"#;
        let result = r.line(line, "known").unwrap();
        assert_eq!(result.error, Some("No structured output returned".to_string()));
    }

    #[test]
    fn reader_marks_limit_hit_from_the_error_text_or_a_429() {
        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"error","is_error":true,"result":"usage limit reached"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(result.limit_hit);
        assert!(!result.ok);

        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"error","api_error_status":429,"result":"no"}"#;
        let result = r.line(line, "known").unwrap();
        assert!(result.limit_hit);
    }

    #[test]
    fn reader_falls_back_to_the_last_assistant_text_when_the_result_has_none() {
        let mut r = CursorReader::new(schema());
        r.line(
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"```json\n{\"ok\":true}\n```"}]}}"#,
            "known",
        );
        let result = r.line(r#"{"type":"result","subtype":"success"}"#, "known").unwrap();
        assert!(result.ok);
        assert_eq!(result.structured, Some(json!({"ok": true})));
    }

    #[test]
    fn reader_maps_usage_to_tokens_and_never_sets_a_cost() {
        let mut r = CursorReader::new(schema());
        let line = r#"{"type":"result","subtype":"success","result":"```json\n{\"ok\":true}\n```","usage":{"inputTokens":8948,"outputTokens":24,"cacheReadTokens":3968}}"#;
        let result = r.line(line, "known").unwrap();
        assert_eq!(
            result.tokens,
            Some(Tokens {
                input: 8948,
                output: 24
            })
        );
        assert_eq!(result.cost_usd, None);

        let mut r = CursorReader::new(schema());
        let result = r
            .line(r#"{"type":"result","subtype":"success","result":"x"}"#, "known")
            .unwrap();
        assert_eq!(result.tokens, None);
    }

    #[test]
    fn no_result_tails_stderr_on_a_char_boundary_and_flags_a_limit_from_its_text() {
        let r = CursorReader::new(schema());
        let stderr = format!("{}{}", "😀", "a".repeat(1499));
        let result = r.no_result(Some(1), &stderr, "known");
        let err = result.error.unwrap();
        assert!(err.starts_with("cursor-agent exited with code 1 and no result."));
        assert!(err.ends_with(&"a".repeat(1499)), "no half emoji kept");
        assert!(!result.limit_hit);

        let limited = r.no_result(Some(1), "rate limit exceeded", "known");
        assert!(limited.limit_hit);
    }

    #[test]
    fn reader_dispatches_through_the_engine_trait() {
        let dir = tempfile::tempdir().unwrap();
        let engine = CursorEngine;
        let spec = base_spec(dir.path());
        assert_eq!(engine.id(), EngineId::Cursor);
        assert_eq!(engine.args(&spec), cursor_args(&spec));
        assert_eq!(engine.env(&spec), cursor_env(&spec));
        assert_eq!(engine.prompt(&spec), cursor_prompt(&spec));
        let mut reader = engine.reader(&spec);
        let line = r#"{"type":"result","subtype":"success","result":"```json\n{}\n```"}"#;
        assert!(
            reader.line(line, "known").unwrap().ok,
            "an empty object satisfies the bare object schema"
        );
    }
}
