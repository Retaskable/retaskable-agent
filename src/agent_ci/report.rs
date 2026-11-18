//! The code host side of agent CI (docs/specs/agent-ci.md sections 3.3 and 3.5): commit statuses, reading them
//! back, and the failure's log comment, all through `gh api` or `glab api`. The program is a parameter so tests use a
//! fake.
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::ci_logs;

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

/// The pending status after the runner itself failed (docker, not the checks); the run is tried again.
pub fn retrying_text(me: &str) -> String {
    format!("{me}: runner trouble, trying again")
}

/// `minutes` is the runner's text for it: tenths (`1.5`).
pub fn passed_text(me: &str, minutes: &str) -> String {
    format!("{me}: passed in {minutes}m")
}

pub fn failed_text(me: &str, minutes: &str, last_error_line: &str) -> String {
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

/// The failure comment, as the runner words it: its head names the check and short sha, then the log's failing
/// lines and last lines.
pub fn failure_comment(me: &str, sha: &str, log: &str) -> String {
    let (lead, excerpt) = ci_logs::comment_log_text(log);
    let short: String = sha.chars().take(8).collect();
    let lines = excerpt.replace("```", "'''");
    format!("{me} failed on {short}. {lead}\n\n```\n{lines}\n```\n")
}

fn str_of<'a>(v: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .unwrap_or("")
}

impl Host {
    pub(super) fn api(&self, method: &str, endpoint: &str, fields: &[(&str, &str)]) -> Result<String> {
        let mut guarded = vec!["api", "-X", method, endpoint];
        let fields: Vec<(&str, String)> = fields.iter().map(|(k, v)| (*k, format!("{k}={v}"))).collect();
        guarded.extend(fields.iter().map(|(_, f)| f.as_str()));
        crate::host_call::guard_program(&self.program, Some(&self.dir), &guarded)?;
        let mut command = crate::spawn::command(&self.program);
        command
            .current_dir(&self.dir)
            .arg("api")
            .args(["-X", method])
            .arg(endpoint);
        for (_, f) in &fields {
            command.arg("-f").arg(f);
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
        let v: Value = serde_json::from_str(&self.get_cached(&endpoint)?)?;
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

    /// Comments the failure on the pull request (`pr`), or on the commit when there is none (`pr` is 0). The link
    /// of the comment the status points at, when the host gave one.
    pub fn post_log_comment(&self, me: &str, pr: u64, sha: &str, log: &str) -> Result<Option<String>> {
        let body = failure_comment(me, sha, log);
        let endpoint = match (self.kind, pr) {
            (Kind::Github, 0) => format!("repos/{}/commits/{sha}/comments", self.repo),
            (Kind::Github, _) => format!("repos/{}/issues/{pr}/comments", self.repo),
            (Kind::Gitlab, 0) => format!("projects/{}/repository/commits/{sha}/comments", self.project()),
            (Kind::Gitlab, _) => format!("projects/{}/merge_requests/{pr}/notes", self.project()),
        };
        let posted = self.api("POST", &endpoint, &[("body", &body)])?;
        let v: Value = serde_json::from_str(&posted).unwrap_or(Value::Null);
        Ok(v.get("html_url").and_then(Value::as_str).map(String::from))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_comment_is_the_runners_with_the_excerpt() {
        let log: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        let c = failure_comment("agent-ci box", "0123456789abcdef", &log);
        let head = "agent-ci box failed on 01234567. The failing lines and last lines of its log:\n\n```\n";
        assert!(c.starts_with(head));
        assert!(c.ends_with("line 100\n```\n") && !c.contains("lines left out"));
        let long: String = (1..=500).map(|i| format!("line {i}\n")).collect();
        assert!(failure_comment("m", "abc", &long).contains("... (440 lines left out)\nline 441\n"));
        let fenced = failure_comment("m", "abc", "```");
        assert_eq!(
            fenced,
            "m failed on abc. The failing lines and last lines of its log:\n\n```\n'''\n```\n"
        );
    }

    #[test]
    fn descriptions_are_the_runners() {
        let m = me("box");
        assert_eq!(running_text(&m), "agent-ci box: running");
        assert_eq!(passed_text(&m, "3.0"), "agent-ci box: passed in 3.0m");
        assert_eq!(failed_text(&m, "4.5", " boom "), "agent-ci box: failed in 4.5m boom");
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
                 *statuses?per_page*) cat statuses.json;; *POST*comments*) cat posted.json;; esac\n",
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
                &passed_text(&m, "2.0"),
                Some("http://x/log"),
            )
            .unwrap();
            h.post_status(
                "abc",
                PUSH_CONTEXT,
                State::Failure,
                &failed_text(&m, "2.0", "bad"),
                None,
            )
            .unwrap();
            let c = calls(d.path());
            assert_eq!(
                c[0],
                "CALL api -X POST repos/o/r/statuses/abc -f state=pending -f context=ci/agent/pr/ci -f description=agent-ci box: running"
            );
            assert!(c[1].ends_with("-f state=success -f context=ci/agent/pr/ci -f description=agent-ci box: passed in 2.0m -f target_url=http://x/log"));
            assert!(c[2].ends_with(
                "-f state=failure -f context=ci/agent/push/ci -f description=agent-ci box: failed in 2.0m bad"
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
        fn a_failure_comment_is_posted_new_and_its_link_returned() {
            let d = tempfile::tempdir().unwrap();
            let h = host(d.path(), Kind::Github);
            std::fs::write(
                d.path().join("posted.json"),
                r#"{"html_url":"https://github.com/o/r/pull/7#c1"}"#,
            )
            .unwrap();
            let link = h.post_log_comment("agent-ci box", 7, "abcdef0123", "boom\n").unwrap();
            assert_eq!(link.as_deref(), Some("https://github.com/o/r/pull/7#c1"));
            h.post_log_comment("agent-ci box", 0, "abcdef0123", "boom\n").unwrap();
            let c = calls(d.path());
            assert!(
                c[0].starts_with(
                    "CALL api -X POST repos/o/r/issues/7/comments -f body=agent-ci box failed on abcdef01."
                )
            );
            assert!(c[1].starts_with("CALL api -X POST repos/o/r/commits/abcdef0123/comments"));
            assert_eq!(c.len(), 2);
        }
    }
}
