//! The trial plan's state (first-run-check.md section 3.3, "In this Rust agent"): one fixed change,
//! started once per project, kept in the project's `state.json` under `trialPlan`. This holds the
//! record, the fixed instruction and the plain-code decisions; nothing runs it yet.
use crate::config::ProjectConfig;
use crate::setup_check::{setup_checks_pass, SetupChecks};

/// The instruction a trial session gets in place of an item's `remaining` text.
pub const TRIAL_TEXT: &str = "Add a line to the README saying this project uses Retaskable. \
Add the one line to the repo's top-level README, creating it if there is none, and change nothing else.";

/// The trial's id, standing where an item's id would.
pub const TRIAL_ID: &str = "_trial";

/// Its branch, built as `orchestrator::session::branch_name` builds others (`retaskable/<id>-<slug>`),
/// with no slice number since it never retries.
pub const TRIAL_BRANCH: &str = "retaskable/_trial-say-hello";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialState {
    #[default]
    NotStarted,
    Running,
    Open,
    Failed,
}

/// `state.trialPlan`. Every field defaults, so an older file with no record reads as not started.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TrialRecord {
    pub state: TrialState,
    pub pr_url: Option<String>,
    /// The backlog already held items, so the trial never fires.
    pub skipped: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Start,
    /// Record it as skipped so it never fires later.
    Skip,
    Wait,
}

impl TrialRecord {
    /// Saved before the session starts, so a restart never starts a second.
    pub fn started() -> Self {
        Self {
            state: TrialState::Running,
            ..Self::default()
        }
    }

    /// The PR is open: the step passes.
    pub fn opened(pr_url: impl Into<String>) -> Self {
        Self {
            state: TrialState::Open,
            pr_url: Some(pr_url.into()),
            skipped: false,
        }
    }

    /// Dropped: no retry.
    pub fn dropped() -> Self {
        Self {
            state: TrialState::Failed,
            ..Self::default()
        }
    }

    pub fn skip() -> Self {
        Self {
            skipped: true,
            ..Self::default()
        }
    }
}

/// Whether the trial starts now. Start needs a repo, passing checks, an empty backlog and a
/// not-started record that was never skipped; a non-empty backlog skips it for good.
pub fn should_start(
    record: &TrialRecord,
    project: &ProjectConfig,
    checks: Option<&SetupChecks>,
    backlog_items: usize,
) -> Decision {
    if record.state != TrialState::NotStarted || record.skipped || project.github_repo.is_empty() {
        return Decision::Wait;
    }
    if !checks.is_some_and(setup_checks_pass) {
        return Decision::Wait;
    }
    if backlog_items > 0 {
        return Decision::Skip;
    }
    Decision::Start
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup_check::SetupCheck;

    fn check(ok: bool) -> SetupCheck {
        SetupCheck {
            ok: Some(ok),
            detail: String::new(),
            fix: None,
        }
    }

    fn checks(ok: bool) -> SetupChecks {
        SetupChecks {
            git: check(ok),
            gh_installed: check(ok),
            gh: check(ok),
            claude: check(ok),
            claude_signed_in: check(ok),
            cursor: check(false),
            pi: None,
            codex: None,
        }
    }

    fn project(repo: &str) -> ProjectConfig {
        ProjectConfig {
            github_repo: repo.to_string(),
            ..ProjectConfig::default()
        }
    }

    #[test]
    fn an_older_record_reads_as_not_started() {
        let r: TrialRecord = serde_json::from_str("{}").unwrap();
        assert_eq!(r, TrialRecord::default());
        assert_eq!(r.state, TrialState::NotStarted);
        let r: TrialRecord = serde_json::from_str(r#"{"state":"open","prUrl":"u","skipped":false}"#).unwrap();
        assert_eq!((r.state, r.pr_url.as_deref()), (TrialState::Open, Some("u")));
    }

    #[test]
    fn starts_only_with_a_repo_passing_checks_an_empty_backlog_and_a_fresh_record() {
        let p = project("a/b");
        let c = checks(true);
        let fresh = TrialRecord::default();
        assert_eq!(should_start(&fresh, &p, Some(&c), 0), Decision::Start);
        assert_eq!(should_start(&fresh, &project(""), Some(&c), 0), Decision::Wait);
        assert_eq!(should_start(&fresh, &p, None, 0), Decision::Wait);
        assert_eq!(should_start(&fresh, &p, Some(&checks(false)), 0), Decision::Wait);
    }

    #[test]
    fn a_non_empty_backlog_skips_and_a_skipped_record_then_always_waits() {
        let p = project("a/b");
        let c = checks(true);
        assert_eq!(should_start(&TrialRecord::default(), &p, Some(&c), 3), Decision::Skip);
        assert_eq!(should_start(&TrialRecord::skip(), &p, Some(&c), 3), Decision::Wait);
        assert_eq!(should_start(&TrialRecord::skip(), &p, Some(&c), 0), Decision::Wait);
    }

    #[test]
    fn every_state_but_not_started_waits_and_none_goes_back() {
        let p = project("a/b");
        let c = checks(true);
        let running = TrialRecord::started();
        assert_eq!(running.state, TrialState::Running);
        let open = TrialRecord::opened("https://x/pr/1");
        assert_eq!(
            (open.state, open.pr_url.as_deref()),
            (TrialState::Open, Some("https://x/pr/1"))
        );
        let failed = TrialRecord::dropped();
        assert_eq!(failed.state, TrialState::Failed);
        for r in [running, open, failed] {
            assert_eq!(should_start(&r, &p, Some(&c), 0), Decision::Wait);
        }
    }

    #[test]
    fn the_branch_and_state_names() {
        assert_eq!(TRIAL_BRANCH, format!("retaskable/{TRIAL_ID}-say-hello"));
        assert_eq!(
            serde_json::to_string(&TrialState::NotStarted).unwrap(),
            "\"not_started\""
        );
    }
}
