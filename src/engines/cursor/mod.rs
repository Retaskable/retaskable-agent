//! The Cursor engine: `cursor-agent -p` with stream-json out, behind the `Engine` interface the
//! runner calls through (docs/specs/engines/02-the-engine-layer.md sections 4 to 6 and 8).
//! Following `agent/src/engines/cursor.ts`, `jsonResult.ts`, `check.ts` and
//! `shared/cursorAllow.ts`/`shared/text.ts` in Retaskable.com (the ground truth this is porting).
//! Split by responsibility: [`allowlist`] (what a session may do, and keeping its own
//! `.cursor/cli.json` sound), [`result`] (turning cursor-agent's own output into a structured
//! result or a transcript line), and [`reader`] (the per-session `EngineReader` built on `result`).
use crate::env::Env;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use anyhow::Result;
use regex::Regex;
use serde_json::Value;

use super::engine::{Engine, EngineId, EngineReader, SessionEvent, SessionSpec, SessionStart};

mod allowlist;
mod reader;
mod result;

use reader::CursorReader;

/// `cursorArgs(spec)`: print mode, `stream-json` out, trusted (never asked to approve), the
/// chosen model (or "auto"), `--mode ask` for a read-only role, and `--resume` for a session
/// that already has a chat (docs/specs/engines/03-choosing-the-engine.md section 3.3: a fresh
/// session's prechosen id is never sent as `--resume` — `cursor-agent` has no way to start a
/// chat under a caller-chosen id, so that id only becomes real once `create-chat` replaces it).
/// Never `--force`/`--yolo` — what a session may do is the allowlist's.
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
        SessionStart::Ephemeral | SessionStart::Fresh(_) => {}
        SessionStart::Resume(id) => {
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

fn dedupe_refs(items: Vec<&str>) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    items.into_iter().filter(|i| seen.insert(*i)).collect()
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

/// The Cursor engine: `cursorArgs`, `cursorEnv`, `cursorPrompt`, `prepareCursor` and the
/// `stream-json` reader, behind the `Engine` interface the runner calls through.
pub struct CursorEngine;

impl Engine for CursorEngine {
    fn id(&self) -> EngineId {
        EngineId::Cursor
    }

    fn binary(&self, env: &Env, windows: bool, path_dirs: &[PathBuf]) -> Result<PathBuf> {
        let home = env.get("HOME").or_else(|| env.get("USERPROFILE")).map(PathBuf::from);
        Ok(super::resolve::resolve_cursor(env, windows, path_dirs, home))
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
        allowlist::prepare_cursor(spec)
    }

    fn reader(&self, spec: &SessionSpec) -> Box<dyn EngineReader> {
        Box::new(CursorReader::new(spec.schema.clone()))
    }

    fn summarize(&self, event: &Value, at: i64) -> Vec<SessionEvent> {
        result::summarize_cursor(event, at)
    }
}

#[cfg(test)]
fn base_spec(cwd: &Path) -> SessionSpec {
    SessionSpec {
        engine: Some(EngineId::Cursor),
        model: "sonnet".to_string(),
        prompt: "plan the board".to_string(),
        schema: serde_json::json!({"type": "object"}),
        cwd: cwd.to_path_buf(),
        max_turns: None,
        budget_usd: 0.0,
        start: SessionStart::Ephemeral,
        readonly: true,
        cursor: None,
        local: None,
        sandbox: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engines::engine::CursorOptions;

    // cursor_args / shell_programs / cursor_prompt

    #[test]
    fn cursor_args_prints_streams_asks_when_read_only_and_never_forces() {
        let dir = tempfile::tempdir().unwrap();
        let mut spec = base_spec(dir.path());
        spec.start = SessionStart::Resume("chat-1".to_string());
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
    fn cursor_args_a_fresh_start_sends_no_resume_cursor_cant_honor_a_prechosen_id() {
        // docs/specs/engines/03-choosing-the-engine.md section 3.3: "A fresh implementer run's
        // prechosen id is not sent as --resume: create-chat replaces it" — cursor-agent has no
        // way to start a chat under a caller-chosen id, so sending it as --resume would try to
        // resume a chat that was never created.
        let dir = tempfile::tempdir().unwrap();
        let mut spec = base_spec(dir.path());
        spec.start = SessionStart::Fresh("prechosen-id".to_string());
        assert!(!cursor_args(&spec).contains(&"--resume".to_string()));
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
