//! docs/specs/agent-ci.md section 3.3: the person's-merge hold. While an agent pull request that is up to date
//! with main is queued or being checked, each person's pull request carries a pending `HOLD_CONTEXT` status, so
//! its merge waits; once none is, the status is released (success). The TypeScript runner holds the same
//! pull requests (`agentTurn` in `.agent-ci/runner.mjs`: a `retaskable/` branch, 0 behind main, its `/ci` status
//! pending); it holds the merge by waiting, this loop by status.
use std::collections::HashSet;

use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;

use super::report::{Host, PR_CONTEXT, State};

/// The context of the hold: the TypeScript pipeline's `turn` step (ci-when-ready.md section 9).
pub const HOLD_CONTEXT: &str = "ci/agent/turn";
/// The branch prefix that marks an agent's pull request, as in the runner.
pub const AGENT_BRANCH_PREFIX: &str = "retaskable/";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pr {
    number: u64,
    #[serde(default)]
    is_draft: bool,
    head_ref_oid: String,
    #[serde(default)]
    head_ref_name: String,
}

pub fn waiting_text(me: &str) -> String {
    format!("{me}: waiting for the agent's turn")
}

pub fn released_text(me: &str) -> String {
    format!("{me}: the agent's turn is over")
}

/// Whether the agent pull request's head is 0 behind `base` (an unreadable answer is not up to date).
fn up_to_date(host: &Host, base: &str, sha: &str) -> bool {
    let endpoint = format!("repos/{}/compare/{base}...{sha}", host.repo);
    host.api("GET", &endpoint, &[])
        .ok()
        .and_then(|out| serde_json::from_str::<Value>(&out).ok())
        .and_then(|v| v.get("behind_by").and_then(Value::as_u64))
        == Some(0)
}

/// Holds or releases each person's pull request in `list` (`gh pr list`'s JSON). `ready` are the numbers of the
/// pull requests this loop checks; an agent one among them with no finished `PR_CONTEXT` status is queued or running.
pub fn apply(host: &Host, base: &str, list: &str, ready: &HashSet<u64>, me: &str) -> Result<()> {
    let prs: Vec<Pr> = serde_json::from_str(list)?;
    let is_agent = |p: &Pr| p.head_ref_name.starts_with(AGENT_BRANCH_PREFIX);
    let mut holding = false;
    for p in prs.iter().filter(|p| is_agent(p) && ready.contains(&p.number)) {
        if !up_to_date(host, base, &p.head_ref_oid) {
            continue;
        }
        let statuses = host.read_statuses(&p.head_ref_oid)?;
        let ci = statuses.iter().find(|s| s.context == PR_CONTEXT);
        if ci.is_none_or(|s| s.state == "pending") {
            holding = true;
            break;
        }
    }
    // A held draft is released too; one whose statuses can't be read is skipped, not left to stop the rest.
    for p in prs.iter().filter(|p| !is_agent(p)) {
        let Ok(statuses) = host.read_statuses(&p.head_ref_oid) else {
            tracing::warn!("merge hold: statuses of PR #{} unreadable", p.number);
            continue;
        };
        let held = statuses
            .iter()
            .find(|s| s.context == HOLD_CONTEXT)
            .is_some_and(|s| s.state == "pending");
        if holding && !held && !p.is_draft {
            host.post_status(&p.head_ref_oid, HOLD_CONTEXT, State::Pending, &waiting_text(me), None)?;
        } else if !holding && held {
            host.post_status(&p.head_ref_oid, HOLD_CONTEXT, State::Success, &released_text(me), None)?;
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::agent_ci::report::Kind;
    use crate::test_support::write_executable;
    use std::path::Path;

    /// A `gh` that logs each call, answers `compare` from `behind.json` and statuses from `<sha>.json`.
    fn host(dir: &Path) -> Host {
        let program = dir.join("fake");
        write_executable(
            &program,
            "#!/bin/sh\n[ -n \"$1\" ] || exit 0\nprintf 'CALL %s\\n' \"$*\" >> calls.log\n\
             case \"$*\" in *compare*) cat behind.json;; \
             *commits/*/statuses*) sha=${4#*commits/}; sha=${sha%%/*}; cat \"$sha.json\" 2>/dev/null || echo '[]';; esac\n",
        );
        if !dir.join("behind.json").exists() {
            std::fs::write(dir.join("behind.json"), r#"{"behind_by":0}"#).unwrap();
        }
        Host {
            program,
            kind: Kind::Github,
            repo: "o/r".into(),
            dir: dir.to_path_buf(),
        }
    }

    const LIST: &str = r#"[
      {"number":1,"isDraft":false,"headRefOid":"agent","headRefName":"retaskable/x"},
      {"number":2,"isDraft":false,"headRefOid":"person","headRefName":"fix-typo"}]"#;

    fn posts(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains("-X POST"))
            .map(String::from)
            .collect()
    }

    fn run(dir: &Path) {
        apply(&host(dir), "main", LIST, &HashSet::from([1]), "agent-ci box").unwrap();
    }

    fn status(state: &str, context: &str) -> String {
        format!(r#"[{{"state":"{state}","context":"{context}"}}]"#)
    }

    #[test]
    fn a_queued_agent_pr_holds_a_person_and_a_finished_one_releases_it() {
        let d = tempfile::tempdir().unwrap();
        host(d.path());
        // Queued: no status on the agent's head yet.
        run(d.path());
        let p = posts(d.path());
        assert_eq!(p.len(), 1);
        assert!(p[0].contains("repos/o/r/statuses/person"));
        assert!(p[0].contains("state=pending") && p[0].contains("context=ci/agent/turn"));
        assert!(p[0].contains("description=agent-ci box: waiting for the agent's turn"));
        // Running: pending, and the person already held: nothing more is posted.
        std::fs::write(d.path().join("agent.json"), status("pending", PR_CONTEXT)).unwrap();
        std::fs::write(d.path().join("person.json"), status("pending", HOLD_CONTEXT)).unwrap();
        run(d.path());
        assert_eq!(posts(d.path()).len(), 1);
        // Checked: released.
        std::fs::write(d.path().join("agent.json"), status("success", PR_CONTEXT)).unwrap();
        run(d.path());
        let p = posts(d.path());
        assert_eq!(p.len(), 2);
        assert!(p[1].contains("state=success") && p[1].contains("context=ci/agent/turn"));
        // Released: a person's success status isn't posted again.
        std::fs::write(d.path().join("person.json"), status("success", HOLD_CONTEXT)).unwrap();
        run(d.path());
        assert_eq!(posts(d.path()).len(), 2);
    }

    #[test]
    fn an_agent_pr_behind_main_holds_no_one() {
        let d = tempfile::tempdir().unwrap();
        host(d.path());
        std::fs::write(d.path().join("behind.json"), r#"{"behind_by":3}"#).unwrap();
        run(d.path());
        assert!(posts(d.path()).is_empty());
    }

    #[test]
    fn an_agent_pr_not_being_checked_holds_no_one() {
        let d = tempfile::tempdir().unwrap();
        host(d.path());
        apply(&host(d.path()), "main", LIST, &HashSet::new(), "m").unwrap();
        assert!(posts(d.path()).is_empty());
    }
}
