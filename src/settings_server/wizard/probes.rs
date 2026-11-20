//! What the wizard asks of the outside world: GitHub through `gh`, Claude Code through `claude`. A trait so every
//! step's check is tested with answers made up; the real one runs the programs (never through a shell), each under a
//! time limit. None of them is ever given a token as an argument: `gh` and `claude` read it from this process's
//! environment, where a saved secret is put.
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde_json::Value;

use super::steps::ClaudeStatus;
use crate::env::Env;

const LIMIT: Duration = Duration::from_secs(25);
/// A `claude -p` call has to start Claude Code and answer: longer.
const CLAUDE_LIMIT: Duration = Duration::from_secs(90);

pub trait Probes: Send + Sync {
    /// `gh api -i user`'s whole output, or why it didn't answer.
    fn gh_user(&self) -> BoxFuture<'static, Result<String, String>>;
    /// Whether the token can list repos (`gh api user/repos?per_page=1`).
    fn gh_repos(&self) -> BoxFuture<'static, Result<(), String>>;
    /// Whether Claude Code is signed in (`claude auth status`).
    fn claude(&self) -> BoxFuture<'static, ClaudeStatus>;
    /// One tiny `claude -p` call on the cheapest model: what it said, or why it didn't.
    fn claude_test(&self) -> BoxFuture<'static, Result<String, String>>;
    /// Sets git's identity from the account where git has none, and says whose commits these are now.
    fn identity(&self, user: Value) -> Option<String>;
}

pub struct RealProbes {
    pub env: Env,
}

struct Out {
    ok: bool,
    stdout: String,
    stderr: String,
}

async fn run(bin: impl AsRef<std::ffi::OsStr>, args: &[&str], limit: Duration) -> Result<Out, String> {
    let mut cmd = crate::spawn::tokio_command(bin);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let out = tokio::time::timeout(limit, child.wait_with_output())
        .await
        .map_err(|_| format!("it didn't answer within {} seconds", limit.as_secs()))?
        .map_err(|e| e.to_string())?;
    Ok(Out {
        ok: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// What `gh` says went wrong, short and with no token in it.
fn gh_words(out: &Out) -> String {
    let said = format!("{}\n{}", out.stderr, out.stdout);
    if crate::gh_limit::is_limited(&said) {
        return crate::gh_limit::message(crate::gh_limit::reset_in(&said, crate::gh_limit::now()));
    }
    if said.contains("401") || said.to_lowercase().contains("bad credentials") {
        return "GitHub refused the token (HTTP 401): it is wrong, expired or revoked. Create a new one.".into();
    }
    if said.contains("403") {
        return "GitHub refused the token (HTTP 403): it may lack a scope or need SSO authorisation for your organisation.".into();
    }
    if said.to_lowercase().contains("authentication") || said.to_lowercase().contains("gh auth login") {
        return crate::profile::NOT_SIGNED_IN.into();
    }
    format!(
        "GitHub didn't answer: {}",
        crate::text::truncate(
            said.lines().find(|l| !l.trim().is_empty()).unwrap_or("no reply").trim(),
            160
        )
    )
}

impl Probes for RealProbes {
    fn gh_user(&self) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async {
            let args = ["api", "-i", "user"];
            crate::host_call::guard_program("gh", None, &args).map_err(|e| e.to_string())?;
            match run("gh", &args, LIMIT).await {
                Ok(out) if out.ok => Ok(out.stdout),
                Ok(out) => Err(gh_words(&out)),
                Err(e) if e.contains("No such file") || e.contains("not found") => {
                    Err("The GitHub CLI (gh) isn't installed in this program's image.".into())
                }
                Err(e) => Err(format!("GitHub didn't answer: {e}")),
            }
        })
    }

    fn gh_repos(&self) -> BoxFuture<'static, Result<(), String>> {
        Box::pin(async {
            let args = ["api", "user/repos?per_page=1"];
            crate::host_call::guard_program("gh", None, &args).map_err(|e| e.to_string())?;
            match run("gh", &args, LIMIT).await {
                Ok(out) if out.ok => Ok(()),
                Ok(out) => Err(format!("The token can't list your repositories. {}", gh_words(&out))),
                Err(e) => Err(format!("GitHub didn't answer: {e}")),
            }
        })
    }

    fn claude(&self) -> BoxFuture<'static, ClaudeStatus> {
        let env = self.env.clone();
        Box::pin(async move {
            let check = crate::setup_check::claude_sign_in_check(&env).await;
            match check.ok {
                Some(true) => ClaudeStatus::SignedIn(check.detail),
                Some(false) if check.detail.contains("installed") => ClaudeStatus::Missing,
                Some(false) => ClaudeStatus::SignedOut,
                None => ClaudeStatus::Unknown(check.detail),
            }
        })
    }

    fn claude_test(&self) -> BoxFuture<'static, Result<String, String>> {
        let env = self.env.clone();
        Box::pin(async move {
            let bin = crate::engines::resolve::resolve_claude_on_path(&env).map_err(|e| e.to_string())?;
            let args = [
                "-p",
                "Reply with the single word ok.",
                "--model",
                "haiku",
                "--max-turns",
                "1",
            ];
            match run(&bin, &args, CLAUDE_LIMIT).await {
                Ok(out) if out.ok && !out.stdout.trim().is_empty() => Ok(crate::text::truncate(out.stdout.trim(), 80)),
                Ok(out) => Err(format!(
                    "Claude Code didn't answer: {}",
                    crate::text::truncate(
                        out.stderr
                            .lines()
                            .chain(out.stdout.lines())
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("no reply")
                            .trim(),
                        160
                    )
                )),
                Err(e) => Err(format!("Claude Code didn't answer: {e}")),
            }
        })
    }

    fn identity(&self, user: Value) -> Option<String> {
        use crate::git_identity::{Git, RealGit, apply, from_user};
        let identity = from_user(&user)?;
        let set = apply(&RealGit, &identity);
        if !set.is_empty() {
            tracing::info!("Set git's {} from the GitHub account.", set.join(" and "));
        }
        let (name, email) = (RealGit.get("user.name")?, RealGit.get("user.email")?);
        Some(format!("{name} <{email}>"))
    }
}
