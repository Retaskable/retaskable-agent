//! docs/specs/agent-ci.md section 3.1 (What is checked): the open pull requests that are ready, each with the
//! key of its check, the pair (head sha, main sha). A new main sha makes a new key.
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;

use super::priority::low_priority_tokio;

/// The label that says a pull request is ready when `ciWhenReady` is on (agent.md section 5).
pub const READY_LABEL: &str = "ci-ready";

/// One check to run: a pull request at a head merged with a main.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CheckKey {
    pub pr: u64,
    pub head: String,
    pub main: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pr {
    number: u64,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    labels: Vec<Label>,
    head_ref_oid: String,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

/// The ready pull requests in `gh pr list --json number,isDraft,labels,headRefOid`'s output: the `ci-ready`
/// ones with `ciWhenReady` active, otherwise every non-draft one.
pub fn ready(json: &str, main: &str, ci_when_ready: bool) -> Result<Vec<CheckKey>, String> {
    let prs: Vec<Pr> = serde_json::from_str(json).map_err(|e| format!("unreadable pull request list: {e}"))?;
    Ok(prs
        .into_iter()
        .filter(|p| {
            if ci_when_ready {
                p.labels.iter().any(|l| l.name == READY_LABEL)
            } else {
                !p.is_draft
            }
        })
        .map(|p| CheckKey {
            pr: p.number,
            head: p.head_ref_oid,
            main: main.to_string(),
        })
        .collect())
}

/// `gh pr list` for the open pull requests into `base`, run in the project's checkout so `gh` uses its login.
pub async fn list(gh: &OsStr, repo_dir: &Path, repo: &str, base: &str) -> Result<String, String> {
    let mut cmd = low_priority_tokio(gh);
    cmd.args([
        "pr", "list", "--repo", repo, "--state", "open", "--base", base, "--limit", "200",
    ])
    .args(["--json", "number,isDraft,labels,headRefOid"])
    .current_dir(repo_dir)
    .stdin(Stdio::null())
    .kill_on_drop(true);
    let out = cmd.output().await.map_err(|e| format!("gh didn't start: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}
