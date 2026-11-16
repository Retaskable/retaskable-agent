//! The `promote` request (`deployRun`'s `promote` case in `agent/src/deployRequests.ts`, docs/specs/agent/deploy-requests.md
//! section 2.3): Deploy to production from the website. The checks of `siteChecks`, then the fast-forward push of
//! the commit named, logged `via: "site"` under whoever pressed it.
use serde_json::{Map, json};

use super::{Outcome, RunError, done};
use crate::backlog::deploy::DeployRequest;
use crate::config::DeployConfig;
use crate::promote::{NOTHING_TO_PROMOTE, Promoter, Stop, merges_to_production};

impl From<Stop> for RunError {
    fn from(s: Stop) -> Self {
        match s {
            Stop::Refused(w) => RunError::Refused(w),
            Stop::Failed(w) => RunError::Failed(w),
        }
    }
}

/// Runs the request and answers `{ sha }`, the commit as asked. A project whose merges go straight to production has
/// nothing to promote, and a request that names no commit is refused, in that order (the TypeScript's).
pub async fn run(promoter: &Promoter, deploy: &DeployConfig, r: &DeployRequest) -> Result<Outcome, RunError> {
    if merges_to_production(deploy) {
        return Err(RunError::Refused(NOTHING_TO_PROMOTE.into()));
    }
    let sha = r
        .args
        .sha
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RunError::Refused("Name a commit.".into()))?;
    promoter.from_site(sha, &r.issued_by).await?;
    let mut m = Map::new();
    m.insert("sha".into(), json!(sha));
    Ok(done(m))
}
