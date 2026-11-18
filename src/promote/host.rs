//! What a promote asks GitHub, through `gh` (`permissions`, `permissionOf` and `login` in `agent/src/gh.ts`): who may
//! promote. Each is an error with `gh`'s own words when `gh` can't say, never a guess.
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

const WORDS_MAX: usize = 300;

/// `gh <args>`'s stdout, trimmed; `gh`'s own words when it fails.
pub(crate) async fn gh(program: &Path, dir: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    crate::host_call::guard_program(program, Some(dir), args).map_err(|e| e.to_string())?;
    let mut cmd = crate::spawn::tokio_command(program);
    cmd.current_dir(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| "gh took too long".to_string())?
        .map_err(|e| format!("starting gh: {e}"))?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
    if out.status.success() {
        return Ok(text(&out.stdout));
    }
    let words = [text(&out.stderr), text(&out.stdout)]
        .into_iter()
        .find(|w| !w.is_empty())
        .unwrap_or_else(|| "gh didn't say".into());
    Err(crate::text::truncate(&words, WORDS_MAX))
}

#[derive(Deserialize)]
struct Permissions {
    #[serde(default)]
    admin: bool,
    #[serde(default)]
    push: bool,
}

/// What this computer's own `gh` login may do on the repo: `(admin, push)` (`gh api repos/<repo> --jq .permissions`).
pub(crate) async fn permissions(program: &Path, dir: &Path, repo: &str) -> Result<(bool, bool), String> {
    let endpoint = format!("repos/{repo}");
    let out = gh(
        program,
        dir,
        &["api", &endpoint, "--jq", ".permissions"],
        Duration::from_secs(30),
    )
    .await?;
    let p: Permissions = serde_json::from_str(&out).map_err(|_| "gh didn't say".to_string())?;
    Ok((p.admin, p.push))
}

/// Someone's permission on the repo: `admin`, `write`, `read` or `none`.
pub(crate) async fn permission_of(program: &Path, dir: &Path, repo: &str, login: &str) -> Result<String, String> {
    let endpoint = format!("repos/{repo}/collaborators/{login}/permission");
    gh(
        program,
        dir,
        &["api", &endpoint, "--jq", ".permission"],
        Duration::from_secs(30),
    )
    .await
}

/// The login `gh` is signed in as (`gh api user`).
pub(crate) async fn login(program: &Path, dir: &Path) -> Result<String, String> {
    gh(
        program,
        dir,
        &["api", "user", "--jq", ".login"],
        Duration::from_secs(15),
    )
    .await
}
