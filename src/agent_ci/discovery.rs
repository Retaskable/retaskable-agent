//! docs/specs/agent-ci.md section 3.1 (Discovery): does the project have agent CI? `.agent-ci/image` and
//! `.agent-ci/checks.sh` are read from `origin/<baseBranch>` with `git show`, never from a pull request, so a
//! pull request can't change what runs its own CI.
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;

use super::priority::low_priority_tokio;

/// `git <args>` in `repo` at low priority: stdout on success, the trimmed stderr otherwise.
pub async fn git(program: &OsStr, repo: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = low_priority_tokio(program);
    cmd.args(args).current_dir(repo).stdin(Stdio::null()).kill_on_drop(true);
    let out = cmd.output().await.map_err(|e| format!("git didn't start: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The project's CI image, or why it has no agent CI. One file missing is the reason.
pub async fn discover(program: &OsStr, repo: &Path, base: &str) -> Result<String, String> {
    let show = |file: &'static str| async move {
        git(program, repo, &["show", &format!("origin/{base}:.agent-ci/{file}")])
            .await
            .ok()
    };
    let image = show("image")
        .await
        .and_then(|s| crate::text::non_blank(&s).map(str::to_string));
    let checks = show("checks.sh").await;
    match (image, checks) {
        (Some(image), Some(_)) => Ok(image),
        (None, Some(_)) => Err(format!("No .agent-ci/image on origin/{base}: no agent CI")),
        (_, None) => Err(format!("No .agent-ci/checks.sh on origin/{base}: no agent CI")),
    }
}
