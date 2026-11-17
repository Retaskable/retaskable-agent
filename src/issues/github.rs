//! GitHub Issues as an issue source (issue-import.md section 3.2), through the agent's own `gh` with arguments, never a
//! shell (`GithubIssues`, `agent/src/issues/github.ts`). The list is the REST API's, not `gh issue list`'s, which has no
//! author association: it is the one Show matches reads (`read_kinds/issues.rs`).
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use super::{Filter, Issue, IssueList, IssueRef, Provider, first_line};
use crate::read_kinds::issues::{Repo, TRUSTED, list};

const GH_TIMEOUT: Duration = Duration::from_secs(30);

pub struct GithubIssues {
    program: PathBuf,
    /// The project's checkout, where `gh` runs.
    dir: PathBuf,
    /// `owner/name`: the pull requests' repo.
    repo: String,
}

impl GithubIssues {
    pub fn new(program: impl Into<PathBuf>, dir: impl Into<PathBuf>, repo: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            dir: dir.into(),
            repo: repo.into(),
        }
    }

    pub async fn list(&self, filter: &Filter) -> Result<IssueList, String> {
        let at = Repo {
            program: &self.program,
            dir: &self.dir,
            repo: &self.repo,
        };
        let Some(found) = list(&at, filter.label.as_deref(), filter.milestone.as_deref()).await? else {
            return Ok(IssueList {
                issues: Vec::new(),
                milestone_missing: true,
            });
        };
        let issues = found
            .into_iter()
            .map(|i| Issue {
                issue: IssueRef {
                    provider: Provider::Github,
                    key: format!("#{}", i.number),
                    url: i.html_url,
                },
                title: i.title,
                body: i.body.unwrap_or_default(),
                author: i.user.map(|u| u.login).unwrap_or_default(),
                trusted: TRUSTED.contains(&i.author_association.as_str()),
            })
            .collect();
        Ok(IssueList {
            issues,
            milestone_missing: false,
        })
    }

    pub async fn comment(&self, issue: &IssueRef, text: &str) -> Result<(), String> {
        let number = issue_number(issue)?;
        self.gh(
            &["issue", "comment", &number, "--repo", &self.repo, "--body-file", "-"],
            Some(text),
        )
        .await
    }

    pub async fn close(&self, issue: &IssueRef) -> Result<(), String> {
        let number = issue_number(issue)?;
        self.gh(
            &["issue", "close", &number, "--repo", &self.repo, "--reason", "completed"],
            None,
        )
        .await
    }

    /// `gh` with these arguments (and `input` on its stdin): `gh`'s first line of words when it fails.
    async fn gh(&self, args: &[&str], input: Option<&str>) -> Result<(), String> {
        crate::host_call::guard_program(&self.program, Some(&self.dir), args).map_err(|e| e.to_string())?;
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.current_dir(&self.dir)
            .args(args)
            .stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().map_err(|e| format!("starting gh: {e}"))?;
        if let (Some(text), Some(mut stdin)) = (input, child.stdin.take()) {
            // A `gh` that exits without reading is its own failure below, not a broken pipe here.
            let _ = stdin.write_all(text.as_bytes()).await;
        }
        let out = tokio::time::timeout(GH_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| "gh took too long".to_string())?
            .map_err(|e| format!("running gh: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_string();
        let words = [text(&out.stderr), text(&out.stdout)]
            .into_iter()
            .find(|w| !w.is_empty())
            .map(|w| first_line(&w))
            .unwrap_or_else(|| format!("gh exited with {}", out.status.code().unwrap_or(-1)));
        Err(words)
    }
}

fn issue_number(issue: &IssueRef) -> Result<String, String> {
    issue
        .key
        .strip_prefix('#')
        .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        .filter(|_| issue.provider == Provider::Github)
        .map(String::from)
        .ok_or_else(|| format!("{} isn't a GitHub issue", issue.key))
}
