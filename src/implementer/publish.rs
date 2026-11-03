//! Publishing a finished session's change (docs/specs/agent/06.2-implementer.md: "The agent pushes the branch and
//! opens its PR ... or finds the one already open for the branch"). Ported from `publishChange` in Retaskable.com's
//! `agent/src/codeHost/index.ts`: push `HEAD` to `origin/<branch>` (never forced), then `gh pr list --head <branch>
//! --state open`, and `gh pr create` only when none is open. The git and gh programs are parameters so tests can
//! use fakes. The body goes to `gh` on stdin (`--body-file -`), so no temp file and no command-line limit.
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// The change a branch was published as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub url: String,
    pub number: u64,
    /// `false` when the branch already had an open PR, so only the push happened.
    pub created: bool,
}

#[derive(Deserialize)]
struct Listed {
    number: u64,
    url: String,
}

pub(crate) fn run(mut cmd: Command, stdin: Option<&str>, what: &str) -> Result<String> {
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().with_context(|| format!("starting {what}"))?;
    if let Some(text) = stdin {
        // A closed pipe (gh failed early) shows up in the exit status below.
        let _ = child.stdin.take().expect("piped").write_all(text.as_bytes());
    }
    let out = child.wait_with_output().with_context(|| format!("running {what}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("{what} failed: {}", err.trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Pushes `dir`'s `HEAD` to `origin/<branch>`, then returns the open PR for the branch, creating it against `base`
/// with `title` and `body` when there is none.
pub fn publish(
    git: &Path,
    gh: &Path,
    dir: &Path,
    branch: &str,
    base: &str,
    title: &str,
    body: &str,
) -> Result<Published> {
    let mut push = Command::new(git);
    push.current_dir(dir)
        .args(["push", "-u", "origin", &format!("HEAD:refs/heads/{branch}")]);
    run(push, None, &format!("git push of {branch}"))?;
    let mut list = Command::new(gh);
    list.current_dir(dir).args([
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "open",
        "--json",
        "number,url",
    ]);
    let listed = run(list, None, "gh pr list")?;
    let open: Vec<Listed> = serde_json::from_str(listed.trim()).context("reading gh pr list's answer")?;
    if let Some(pr) = open.into_iter().next() {
        return Ok(Published {
            url: pr.url,
            number: pr.number,
            created: false,
        });
    }
    let mut create = Command::new(gh);
    create.current_dir(dir).args([
        "pr",
        "create",
        "--base",
        base,
        "--head",
        branch,
        "--title",
        title,
        "--body-file",
        "-",
    ]);
    let made = run(create, Some(body), "gh pr create")?;
    let url = made
        .lines()
        .map(str::trim)
        .rev()
        .find(|l| l.starts_with("http"))
        .context("gh pr create printed no URL")?
        .to_string();
    let number = url
        .rsplit('/')
        .next()
        .and_then(|n| n.parse().ok())
        .with_context(|| format!("no PR number in {url}"))?;
    Ok(Published {
        url,
        number,
        created: true,
    })
}
