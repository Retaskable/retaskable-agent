//! Whether the repo isn't public on GitHub (`repoPrivate` in `agent/src/gh.ts`).
use serde::Deserialize;

#[derive(Deserialize)]
struct View {
    visibility: Option<serde_json::Value>,
}

/// `gh repo view <repo> --json visibility`'s answer as "is it private": private and internal both are; `None` when
/// `gh` can't say (not signed in, no such repo, an answer that isn't a visibility).
pub fn private_from(stdout: &[u8]) -> Option<bool> {
    let view: View = serde_json::from_slice(stdout).ok()?;
    view.visibility?.as_str().map(|v| v != "PUBLIC")
}

pub async fn repo_private(github_repo: &str) -> Option<bool> {
    let args = ["repo", "view", github_repo, "--json", "visibility"];
    crate::host_call::guard_program("gh", None, &args).ok()?;
    let out = crate::spawn::tokio_command("gh").args(args).output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    private_from(&out.stdout)
}
