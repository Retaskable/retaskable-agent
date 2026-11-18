//! `read_file { path }` (`readRepoFile` in the TS agent's `repoFiles.ts`): a Markdown file from the base branch, for the
//! site's file reader and the MCP `read_spec`. `.md` only, no `..`; answered `{ path, ref, content }`.
use serde_json::{Value, json};

use super::ReadError;
use crate::config::ProjectConfig;

/// `/^[\w./ -]+\.md$/i` and no `..` segment: the same check as `GET /api/files` and the command's schema.
fn markdown_path(path: &str) -> bool {
    let chars_ok = path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | ' ' | '-'));
    let stem = path.len() > 3 && path.to_ascii_lowercase().ends_with(".md");
    chars_ok && stem && !path.split('/').any(|part| part == "..")
}

pub fn read_file(project: &ProjectConfig, args: &Value) -> Result<Value, ReadError> {
    if project.repo_path.is_empty() {
        return Err(ReadError::Refused("Finish setup first.".into()));
    }
    let Some(path) = args.get("path").and_then(Value::as_str).filter(|p| markdown_path(p)) else {
        return Err(ReadError::Refused(
            "Only Markdown files in the repo can be read.".into(),
        ));
    };
    let reference = format!("origin/{}", project.base_branch);
    let shown = crate::git_bin::command(std::path::Path::new(&project.repo_path))
        .args(["show", &format!("{reference}:{path}")])
        .output()
        .map_err(|e| ReadError::Failed(format!("starting git: {e}")))?;
    if !shown.status.success() {
        return Err(ReadError::Refused(format!("{path} isn't on {reference}.")));
    }
    Ok(json!({ "path": path, "ref": reference, "content": String::from_utf8_lossy(&shown.stdout) }))
}
