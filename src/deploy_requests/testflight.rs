//! The `testflight_build` request (`deployCan` and `deployRun`'s `testflight_build` case in `agent/src/deployRequests.ts`,
//! docs/specs/agent/deploy-requests.md section 2.4): build Retaskable's own iOS app and upload it to TestFlight. The
//! refusals, the throwaway checkout and the commands are [`crate::testflight`]; this is the request's two edges, whether
//! this computer claims it and how the build's end becomes the request's.
use serde_json::{Map, json};

use super::{Outcome, RunError};
use crate::backlog::deploy::{DeployRequest, Finish};
use crate::config::ProjectConfig;
use crate::promote::DeployLog;
use crate::testflight::{BuildResult, Environment, Tools, check_and_build, ready};

/// Whether this computer claims the request: the setting is on and it is a Mac with Xcode's project and the script,
/// and (unless Any member's computer is on) its code host login is the issuer's, compared case-insensitively. A computer
/// that can't say who it is signed in as leaves it to the build's own admin check.
pub async fn can<T: Tools>(project: &ProjectConfig, tools: &T, r: &DeployRequest) -> bool {
    if !ready(project, tools.mac()) {
        return false;
    }
    if project.release.testflight_any_member {
        return true;
    }
    match tools.login().await {
        Ok(owner) if !owner.is_empty() => owner.to_lowercase() == r.issued_by.to_lowercase(),
        _ => true,
    }
}

/// Runs the build and answers `{ sha, buildNumber, result }`: `done` only when the build ended `submitted`, else
/// `failed` with the same result. A request that names no commit is refused first, then the build's own refusals.
pub async fn run<T: Tools>(
    project: &ProjectConfig,
    tools: &T,
    log: &DeployLog,
    project_dir: &std::path::Path,
    r: &DeployRequest,
) -> Result<Outcome, RunError> {
    let sha = r
        .args
        .sha
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RunError::Refused("Name a commit.".into()))?;
    let environment = Environment::of(r.args.environment.as_deref());
    let out = check_and_build(project, tools, log, project_dir, sha, &r.issued_by, environment).await?;
    let mut result = Map::new();
    result.insert("sha".into(), json!(out.sha));
    result.insert("buildNumber".into(), json!(out.build_number));
    result.insert("result".into(), json!(out.result.id()));
    Ok(Outcome {
        status: if out.result == BuildResult::Submitted {
            Finish::Done
        } else {
            Finish::Failed
        },
        result,
    })
}
