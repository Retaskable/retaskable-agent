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

/// The first open PR for `branch`, if any.
fn open_change(gh: &Path, dir: &Path, branch: &str) -> Result<Option<Listed>> {
    let args = [
        "pr",
        "list",
        "--head",
        branch,
        "--state",
        "open",
        "--json",
        "number,url",
    ];
    crate::host_call::guard_program(gh, Some(dir), &args)?;
    let mut list = Command::new(gh);
    list.current_dir(dir).args(args);
    let listed = run(list, None, "gh pr list")?;
    let open: Vec<Listed> = serde_json::from_str(listed.trim()).context("reading gh pr list's answer")?;
    Ok(open.into_iter().next())
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
    open_or_create(gh, dir, branch, base, title, body)
}

/// The open PR for `branch`, or a new one against `base` with `title` and `body`.
pub fn open_or_create(gh: &Path, dir: &Path, branch: &str, base: &str, title: &str, body: &str) -> Result<Published> {
    if let Some(pr) = open_change(gh, dir, branch)? {
        return Ok(Published {
            url: pr.url,
            number: pr.number,
            created: false,
        });
    }
    let args = [
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
    ];
    crate::host_call::guard_program(gh, Some(dir), &args)?;
    let mut create = Command::new(gh);
    create.current_dir(dir).args(args);
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

/// Closes the open PR for `branch`, unmerged, with `comment` (docs/specs/agent/06.2-implementer.md, `nothing_left`:
/// "closes any pull request still open for the item"). Never deletes the branch. `gh pr close` has no comment-file
/// form, so the comment goes in `--comment`. Returns whether a PR was closed; none open is fine.
pub fn close_change(gh: &Path, dir: &Path, branch: &str, comment: &str) -> Result<bool> {
    let Some(pr) = open_change(gh, dir, branch)? else {
        return Ok(false);
    };
    let n = pr.number.to_string();
    let args = ["pr", "close", n.as_str(), "--comment", comment];
    crate::host_call::guard_program(gh, Some(dir), &args)?;
    let mut close = Command::new(gh);
    close.current_dir(dir).args(args);
    run(close, None, "gh pr close")?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A fake `gh` that appends its arguments to `calls.log`, prints `list.json` for `pr list`, and fails when
    /// `fail` exists.
    fn fake_gh(dir: &Path, listed: &str) -> PathBuf {
        std::fs::write(dir.join("list.json"), listed).unwrap();
        let (log, list, fail) = (dir.join("calls.log"), dir.join("list.json"), dir.join("fail"));
        let path = dir.join(if cfg!(windows) { "fake_gh.cmd" } else { "fake_gh.sh" });
        if cfg!(windows) {
            let script = format!(
                "@echo off\r\necho %* >> \"{}\"\r\nif exist \"{}\" (echo boom 1>&2 & exit /b 1)\r\necho %* | findstr /C:\"pr list\" >nul && type \"{}\"\r\n",
                log.display(),
                fail.display(),
                list.display()
            );
            std::fs::write(&path, script).unwrap();
        } else {
            let script = format!(
                "#!/bin/sh\necho \"$*\" >> \"{}\"\n[ -e \"{}\" ] && {{ echo boom >&2; exit 1; }}\ncase \"$*\" in \"pr list\"*) cat \"{}\";; esac\n",
                log.display(),
                fail.display(),
                list.display()
            );
            crate::test_support::write_executable(&path, &script);
        }
        path
    }

    fn calls(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect()
    }

    #[test]
    fn close_change_closes_the_open_pr_with_the_comment() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), r#"[{"number":7,"url":"https://x/pull/7"}]"#);
        assert!(close_change(&gh, dir.path(), "feat", "Nothing left to build here: done").unwrap());
        let calls = calls(dir.path());
        assert_eq!(calls.len(), 2);
        assert!(calls[0].starts_with("pr list --head feat --state open"));
        assert!(calls[1].starts_with("pr close 7 --comment Nothing left to build here: done"));
        assert!(
            !calls
                .iter()
                .any(|c| c.contains("--delete-branch") || c.contains("merge"))
        );
    }

    #[test]
    fn close_change_with_no_open_pr_only_lists() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), "[]");
        assert!(!close_change(&gh, dir.path(), "feat", "c").unwrap());
        let calls = calls(dir.path());
        assert_eq!(calls.len(), 1);
        assert!(calls[0].starts_with("pr list"));
    }

    #[test]
    fn close_change_names_the_failing_step() {
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(dir.path(), "[]");
        std::fs::write(dir.path().join("fail"), "").unwrap();
        let err = close_change(&gh, dir.path(), "feat", "c").unwrap_err().to_string();
        assert!(err.contains("gh pr list failed"), "{err}");
    }
}
