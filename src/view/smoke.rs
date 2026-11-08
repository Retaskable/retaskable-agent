//! The snapshot's `smoke` (agent/09.2-what-the-computer-reports.md): the staging smoke's line, with `snapshot-reporting` On.
use std::path::Path;

use serde_json::Value;

use super::AgentView;
use crate::paths;
use crate::store::{SmokeStatus, Store};

impl AgentView {
    /// The snapshot's `smoke` (agent/09.2): the project's `State.smoke`, with `snapshot-reporting` On. `tail` only
    /// goes up for a failure or timeout; none when the project has no smoke.
    pub(crate) fn smoke_json(&self, project: &str, data_dir: &Path) -> Option<Value> {
        if !self.settings.snapshot_reporting_on() || !self.projects.iter().any(|p| p.id == project) {
            return None;
        }
        let mut smoke = Store::load(&paths::project_dir(data_dir, project)).ok()?.smoke()?;
        if !matches!(smoke.status, SmokeStatus::Failed | SmokeStatus::Timeout) {
            smoke.tail = None;
        }
        serde_json::to_value(smoke).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn snapshot_reports_the_smoke_line_with_a_tail_only_for_a_failure_or_timeout() {
        use crate::store::SmokeState;
        let dir = tempfile::tempdir().unwrap();
        let smoke = |dir: &Path| {
            super::super::tests::reporting_view(true).snapshot("a", dir)["state"]
                .get("smoke")
                .cloned()
        };
        assert_eq!(smoke(dir.path()), None, "no smoke");
        let mut store = Store::load(&paths::project_dir(dir.path(), "a")).unwrap();
        let mut put = |status, tail: Option<&str>| {
            let s = SmokeState {
                sha: "abc".into(),
                status,
                at: "2026-11-08T00:00:00.000Z".into(),
                url: Some("https://staging.example.com".into()),
                tail: tail.map(String::from),
            };
            store.save_smoke(&s).unwrap();
        };
        put(SmokeStatus::Passed, Some("noise"));
        assert_eq!(
            smoke(dir.path()),
            Some(
                json!({ "sha": "abc", "status": "passed", "at": "2026-11-08T00:00:00.000Z", "url": "https://staging.example.com" })
            )
        );
        put(SmokeStatus::Failed, Some("boom"));
        assert_eq!(smoke(dir.path()).unwrap()["tail"], "boom");
        let off = super::super::tests::reporting_view(false).snapshot("a", dir.path())["state"]
            .get("smoke")
            .cloned();
        assert_eq!(off, None, "flag Off");
    }
}
