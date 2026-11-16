//! The git calls a promote makes, in the project's main checkout.
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

/// How a git call ended.
pub(super) struct Out {
    pub ok: bool,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Runs `git <args>` in `dir`, cut (the process killed) after `timeout`.
pub(super) async fn run(dir: &Path, args: &[&str], timeout: Duration) -> Result<Out, String> {
    let mut cmd = tokio::process::Command::from(crate::git_bin::command(dir));
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| format!("starting git: {e}"))?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(out)) => Ok(Out {
            ok: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).trim().to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
            timed_out: false,
        }),
        Ok(Err(e)) => Err(format!("running git: {e}")),
        Err(_) => Ok(Out {
            ok: false,
            stdout: String::new(),
            stderr: String::new(),
            timed_out: true,
        }),
    }
}

const QUICK: Duration = Duration::from_secs(60);

/// `git fetch --prune origin`, now (never skipped as fresh): the push is decided on what origin shows.
pub(super) async fn fetch(dir: &Path) -> Result<(), String> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || crate::lanes::fetch_now(&dir))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("{e:#}"))
}

/// The commit `sha` names, in full; empty when it isn't one here.
pub(super) async fn full_sha(dir: &Path, sha: &str) -> Result<String, String> {
    let spec = format!("{sha}^{{commit}}");
    Ok(run(dir, &["rev-parse", "--verify", "--quiet", &spec], QUICK)
        .await?
        .stdout)
}

/// `git rev-parse <rev>`, empty when it can't say.
pub(super) async fn rev(dir: &Path, rev: &str) -> Result<String, String> {
    Ok(run(dir, &["rev-parse", rev], QUICK).await?.stdout)
}

/// `git merge-base --is-ancestor a b`: false for anything but a yes (a ref that isn't there refuses, never allows).
pub(super) async fn is_ancestor(dir: &Path, a: &str, b: &str) -> Result<bool, String> {
    Ok(run(dir, &["merge-base", "--is-ancestor", a, b], QUICK).await?.ok)
}

/// The arguments of the one push a promote makes: `origin <sha>:refs/heads/<production>`. No force of any kind, and
/// no `+` on the refspec (it starts with the commit), so git itself refuses anything but a fast-forward.
pub(super) fn push_args(full: &str, production: &str) -> Vec<String> {
    vec![
        "push".into(),
        "origin".into(),
        format!("{full}:refs/heads/{production}"),
    ]
}
