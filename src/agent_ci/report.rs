//! The code host side of agent CI (docs/specs/agent-ci.md sections 3.3 and 3.5): commit statuses, reading them
//! back, and the failure comment, all through `gh api` or `glab api`. The program is a parameter so tests use a
//! fake.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::ci_watch::LOG_TAIL_LINES;

// Copied from Retaskable.com's `.agent-ci/runner.mjs` (the TypeScript runner), so the orchestrator's wait is
// unchanged: the contexts it posts, its `ME` (`agent-ci <computer>`) and its three descriptions.
/// The context of a pull request's check.
pub const PR_CONTEXT: &str = "ci/agent/pr/ci";
/// The context of a push to main's check.
pub const PUSH_CONTEXT: &str = "ci/agent/push/ci";
/// A status description is cut to this many characters.
const DESCRIPTION_MAX: usize = 140;

/// The runner's name for a computer: `agent-ci <computer>`.
pub fn me(computer: &str) -> String {
    format!("agent-ci {computer}")
}

pub fn running_text(me: &str) -> String {
    format!("{me}: running")
}

pub fn passed_text(me: &str, minutes: u64) -> String {
    format!("{me}: passed in {minutes}m")
}

pub fn failed_text(me: &str, minutes: u64, last_error_line: &str) -> String {
    format!("{me}: failed in {minutes}m {}", last_error_line.trim())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Pending,
    Success,
    Failure,
}

impl State {
    fn github(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Success => "success",
            State::Failure => "failure",
        }
    }
    fn gitlab(self) -> &'static str {
        match self {
            State::Pending => "pending",
            State::Success => "success",
            State::Failure => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Github,
    Gitlab,
}

/// One commit status, as read back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub context: String,
    /// `pending`, `success` or `failure` (GitLab's `failed` is `failure`).
    pub state: String,
    pub description: String,
    pub creator: String,
    pub updated_at: String,
}

pub struct Host {
    /// `gh` or `glab`.
    pub program: PathBuf,
    pub kind: Kind,
    /// `owner/repo` (GitLab: the project's full path).
    pub repo: String,
    /// Where the program runs.
    pub dir: PathBuf,
}

/// The hidden marker that carries a check key in the failure comment.
pub fn marker(key: &str) -> String {
    format!("<!-- agent-ci-key:{key} -->")
}

/// The failure comment: its head names the check and short sha, then the last 120 log lines, then the marker.
pub fn failure_comment(name: &str, sha: &str, key: &str, log: &str) -> String {
    let lines: Vec<&str> = log.trim_end().lines().collect();
    let tail = lines[lines.len().saturating_sub(LOG_TAIL_LINES)..]
        .join("\n")
        .replace("```", "'''");
    let short: String = sha.chars().take(8).collect();
    format!("{name} failed on {short}.\n\n```\n{tail}\n```\n{}\n", marker(key))
}

fn str_of<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .unwrap_or("")
}

impl Host {
    pub(super) fn api(&self, method: &str, endpoint: &str, fields: &[(&str, &str)]) -> Result<String> {
        let mut command = Command::new(&self.program);
        command
            .current_dir(&self.dir)
            .arg("api")
            .args(["-X", method])
            .arg(endpoint);
        for (k, v) in fields {
            command.arg("-f").arg(format!("{k}={v}"));
        }
        let out = command
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("starting {}", Path::new(&self.program).display()))?;
        if !out.status.success() {
            bail!(
                "{} api {endpoint}: {}",
                self.program.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn project(&self) -> String {
        self.repo.replace('/', "%2F")
    }

    /// Posts a commit status; `target_url` (the log) only when given.
    pub fn post_status(
        &self,
        sha: &str,
        context: &str,
        state: State,
        description: &str,
        target_url: Option<&str>,
    ) -> Result<()> {
        let description: String = description.chars().take(DESCRIPTION_MAX).collect();
        let (endpoint, state_key, context_key, value) = match self.kind {
            Kind::Github => (
                format!("repos/{}/statuses/{sha}", self.repo),
                "state",
                "context",
                state.github(),
            ),
            Kind::Gitlab => (
                format!("projects/{}/statuses/{sha}", self.project()),
                "state",
                "name",
                state.gitlab(),
            ),
        };
        let mut fields = vec![
            (state_key, value),
            (context_key, context),
            ("description", description.as_str()),
        ];
        if let Some(url) = target_url {
            fields.push(("target_url", url));
        }
        self.api("POST", &endpoint, &fields).map(|_| ())
    }

    /// A commit's statuses, newest first as the host lists them.
    pub fn read_statuses(&self, sha: &str) -> Result<Vec<Status>> {
        let endpoint = match self.kind {
            Kind::Github => format!("repos/{}/commits/{sha}/statuses?per_page=100", self.repo),
            Kind::Gitlab => format!(
                "projects/{}/repository/commits/{sha}/statuses?per_page=100",
                self.project()
            ),
        };
        let v: Value = serde_json::from_str(&self.api("GET", &endpoint, &[])?)?;
        let rows = v.as_array().ok_or_else(|| anyhow!("statuses: not a list"))?;
        Ok(rows
            .iter()
            .map(|r| {
                let state = match str_of(r, &["state", "status"]) {
                    "failed" | "error" => "failure",
                    s => s,
                };
                let creator = r.get("creator").or_else(|| r.get("author"));
                Status {
                    context: str_of(r, &["context", "name"]).to_string(),
                    state: state.to_string(),
                    description: str_of(r, &["description"]).to_string(),
                    creator: creator
                        .map(|c| str_of(c, &["login", "username"]))
                        .unwrap_or("")
                        .to_string(),
                    updated_at: str_of(r, &["updated_at", "finished_at", "created_at"]).to_string(),
                }
            })
            .collect())
    }

    /// Comments the failure on the pull request (`pr`): edits this key's earlier comment if there is one, else
    /// adds a new one.
    pub fn post_failure_comment(&self, pr: u64, name: &str, sha: &str, key: &str, log: &str) -> Result<()> {
        let body = failure_comment(name, sha, key, log);
        let (list, base) = match self.kind {
            Kind::Github => (
                format!("repos/{}/issues/{pr}/comments?per_page=100", self.repo),
                format!("repos/{}/issues", self.repo),
            ),
            Kind::Gitlab => {
                let p = format!("projects/{}/merge_requests/{pr}/notes", self.project());
                (format!("{p}?per_page=100"), p)
            }
        };
        let rows: Value = serde_json::from_str(&self.api("GET", &list, &[])?)?;
        let mark = marker(key);
        let existing = rows
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| str_of(c, &["body"]).contains(&mark))
            .and_then(|c| c.get("id").and_then(Value::as_u64));
        let (method, endpoint) = match (self.kind, existing) {
            (Kind::Github, Some(id)) => ("PATCH", format!("repos/{}/issues/comments/{id}", self.repo)),
            (Kind::Github, None) => ("POST", format!("{base}/{pr}/comments")),
            (Kind::Gitlab, Some(id)) => ("PUT", format!("{base}/{id}")),
            (Kind::Gitlab, None) => ("POST", base),
        };
        self.api(method, &endpoint, &[("body", &body)]).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_keeps_the_last_120_lines_and_the_marker() {
        let log: String = (1..=300).map(|i| format!("line {i}\n")).collect();
        let c = failure_comment("agent-ci box", "0123456789abcdef", "k1", &log);
        assert!(c.starts_with("agent-ci box failed on 01234567."));
        assert!(c.contains("line 181\n") && !c.contains("line 180\n") && c.contains("line 300\n"));
        assert!(c.contains(&marker("k1")));
        assert_eq!(c.lines().filter(|l| l.starts_with("line ")).count(), 120);
    }

    #[test]
    fn descriptions_are_the_runners() {
        let m = me("box");
        assert_eq!(running_text(&m), "agent-ci box: running");
        assert_eq!(passed_text(&m, 3), "agent-ci box: passed in 3m");
        assert_eq!(failed_text(&m, 4, " boom "), "agent-ci box: failed in 4m boom");
    }

    #[cfg(unix)]
    mod fake {
        use super::*;
        use crate::test_support::write_executable;

        /// A program that logs each call as `CALL <args>` and answers listings from files in `dir`.
        fn host(dir: &Path, kind: Kind) -> Host {
            let program = dir.join("fake");
            write_executable(
                &program,
                "#!/bin/sh\n[ -n \"$1\" ] || exit 0\nprintf 'CALL %s\\n' \"$*\" | head -1 >> calls.log\n\
                 case \"$*\" in *comments?per_page*|*notes?per_page*) cat comments.json;; \
                 *statuses?per_page*) cat statuses.json;; esac\n",
            );
            std::fs::write(dir.join("comments.json"), "[]").unwrap();
            Host {
                program,
                kind,
                repo: "o/r".into(),
                dir: dir.to_path_buf(),
            }
        }

        fn calls(dir: &Path) -> Vec<String> {
            std::fs::read_to_string(dir.join("calls.log"))
                .unwrap_or_default()
                .lines()
                .map(String::from)
                .collect()
        }

        #[test]
        fn github_statuses_use_the_contexts_and_descriptions() {
            let d = tempfile::tempdir().unwrap();
            let h = host(d.path(), Kind::Github);
            let m = me("box");
            h.post_status("abc", PR_CONTEXT, State::Pending, &running_text(&m), None)
                .unwrap();
            h.post_status(
                "abc",
                PR_CONTEXT,
                State::Success,
                &passed_text(&m, 2),
                Some("http://x/log"),
            )
            .unwrap();
            h.post_status("abc", PUSH_CONTEXT, State::Failure, &failed_text(&m, 2, "bad"), None)
                .unwrap();
            let c = calls(d.path());
            assert_eq!(
                c[0],
                "CALL api -X POST repos/o/r/statuses/abc -f state=pending -f context=ci/agent/pr/ci -f description=agent-ci box: running"
            );
            assert!(c[1].ends_with("-f state=success -f context=ci/agent/pr/ci -f description=agent-ci box: passed in 2m -f target_url=http://x/log"));
            assert!(c[2].ends_with(
                "-f state=failure -f context=ci/agent/push/ci -f description=agent-ci box: failed in 2m bad"
            ));
        }

        #[test]
        fn gitlab_statuses_use_name_and_failed() {
            let d = tempfile::tempdir().unwrap();
            let h = host(d.path(), Kind::Gitlab);
            h.post_status("abc", PR_CONTEXT, State::Failure, "d", None).unwrap();
            assert_eq!(
                calls(d.path())[0],
                "CALL api -X POST projects/o%2Fr/statuses/abc -f state=failed -f name=ci/agent/pr/ci -f description=d"
            );
        }

        #[test]
        fn reads_creator_description_and_updated_at() {
            let d = tempfile::tempdir().unwrap();
            let h = host(d.path(), Kind::Github);
            std::fs::write(
                d.path().join("statuses.json"),
                r#"[{"context":"ci/agent/pr/ci","state":"pending","description":"agent-ci box: running","updated_at":"2026-11-08T10:00:00Z","creator":{"login":"bot"}}]"#,
            )
            .unwrap();
            let s = h.read_statuses("abc").unwrap();
            assert_eq!(
                s,
                vec![Status {
                    context: "ci/agent/pr/ci".into(),
                    state: "pending".into(),
                    description: "agent-ci box: running".into(),
                    creator: "bot".into(),
                    updated_at: "2026-11-08T10:00:00Z".into(),
                }]
            );
        }

        #[test]
        fn a_second_failure_for_a_key_edits_its_comment() {
            let d = tempfile::tempdir().unwrap();
            let h = host(d.path(), Kind::Github);
            let log: String = (1..=300).map(|i| format!("line {i}\n")).collect();
            h.post_failure_comment(7, "agent-ci box", "abc", "k1", &log).unwrap();
            let mut c = calls(d.path());
            assert!(c[1].starts_with("CALL api -X POST repos/o/r/issues/7/comments"));
            let existing = serde_json::json!([{"id": 55, "body": failure_comment("agent-ci box", "abc", "k1", &log)}]);
            std::fs::write(d.path().join("comments.json"), existing.to_string()).unwrap();
            h.post_failure_comment(7, "agent-ci box", "abc", "k1", &log).unwrap();
            h.post_failure_comment(7, "agent-ci box", "def", "k2", &log).unwrap();
            c = calls(d.path());
            assert!(
                c.iter()
                    .any(|l| l.starts_with("CALL api -X PATCH repos/o/r/issues/comments/55"))
            );
            assert_eq!(c.iter().filter(|l| l.contains("-X POST")).count(), 2);
            assert_eq!(c.iter().filter(|l| l.contains("-X PATCH")).count(), 1);
        }
    }
}
