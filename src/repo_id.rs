//! GitHub's numeric repo id, which every program names a project's backlog by.

/// GitHub's numeric repo id (`gh api repos/<repo> --jq .id`, `repoId` in `agent/src/gh.ts`), which names the
/// project's backlog in the store.
pub async fn repo_id(github_repo: &str) -> Option<u64> {
    let path = format!("repos/{github_repo}");
    let args = ["api", path.as_str(), "--jq", ".id"];
    crate::host_call::guard_program("gh", None, &args).ok()?;
    let out = crate::spawn::tokio_command("gh").args(args).output().await.ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .ok()
        .filter(|&n| n > 0)
}
