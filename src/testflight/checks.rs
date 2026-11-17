//! The refusals before a build, in order (`checkRefusals` in `agent/src/testflight.ts`, testflight-release.md section
//! 3.1): nothing is run before they pass, and the first failing one refuses. Anything that can't be read refuses.
use super::{Tools, setup, version_url_of};
use crate::config::ProjectConfig;
use crate::promote::{Stop, merges_to_production};

/// How many times the served commit is read: while a rollout serves two versions the answers alternate.
const SERVED_READS: usize = 5;

/// Which server the build's app talks to, and so which commit it must be: what production serves, or what staging serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Staging,
    Production,
}

impl Environment {
    /// `staging` as the request says it; anything else, or nothing, is production.
    pub fn of(asked: Option<&str>) -> Self {
        if asked == Some("staging") {
            Self::Staging
        } else {
            Self::Production
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Staging => "Staging",
            Self::Production => "Production",
        }
    }
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// What the environment serves: the first read starting with `sha`; else the commit answered most (the last on a tie),
/// or `None` when nothing answered (`servedMatching`).
async fn served_matching<T: Tools>(tools: &T, url: &str, sha: &str) -> Option<String> {
    let want = sha.to_lowercase();
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..SERVED_READS {
        let Some(got) = tools.served(url).await.filter(|g| !g.is_empty()) else {
            continue;
        };
        if got.to_lowercase().starts_with(&want) {
            return Some(got);
        }
        seen.push(got);
    }
    let (mut best, mut most) = (None, 0);
    for s in &seen {
        let n = seen.iter().filter(|x| *x == s).count();
        if n >= most {
            (best, most) = (Some(s.clone()), n);
        }
    }
    best
}

/// The sha the environment serves, in full, once every refusal passed; else the first one, as [`Stop::Refused`].
pub async fn check<T: Tools>(
    project: &ProjectConfig,
    sha: &str,
    issued_by: &str,
    environment: Environment,
    tools: &T,
) -> Result<String, Stop> {
    let name = environment.name();
    if !project.release.testflight {
        return Err(Stop::refused(
            "TestFlight builds are off on this computer: turn them on in its settings.",
        ));
    }
    if let Some(why) = setup::setup_refusal(project, tools.mac()) {
        return Err(Stop::refused(why));
    }
    if !crate::promote::site::is_login(issued_by) {
        return Err(Stop::refused("Only the repo's admins can build for TestFlight."));
    }
    let permission = tools.permission_of(issued_by).await.map_err(|e| {
        Stop::refused(format!(
            "Couldn't ask GitHub whether @{issued_by} is an admin of {}: {e}",
            project.github_repo
        ))
    })?;
    if permission != "admin" {
        let has = if permission.is_empty() || permission == "none" {
            "no access"
        } else {
            &permission
        };
        return Err(Stop::refused(format!(
            "Only the repo's admins can build for TestFlight (@{issued_by} has {has})."
        )));
    }
    // Under Production only there's no staging: its saved address is only kept for switching back, never built from.
    if environment == Environment::Staging && merges_to_production(&project.deploy) {
        return Err(Stop::refused(
            "This project's merges go straight to production: build production.",
        ));
    }
    let Some(url) = version_url_of(project, environment) else {
        return Err(Stop::refused(format!(
            "{name} has no address to check: set it in Settings → Project settings."
        )));
    };
    let Some(serving) = served_matching(tools, &url, sha).await else {
        return Err(Stop::refused(format!(
            "{name} doesn't say which commit it serves at {url}."
        )));
    };
    if !serving.to_lowercase().starts_with(&sha.to_lowercase()) {
        return Err(Stop::refused(format!(
            "{name} serves {}, not {}: build that one.",
            short(&serving),
            short(sha)
        )));
    }
    let version = super::Step::new("xcodebuild", &["-version"], std::path::Path::new(&project.repo_path));
    if !tools.run(&version, None).await.ok() {
        return Err(Stop::refused(
            "Xcode isn't ready on this computer: install it and open it once to accept its license.",
        ));
    }
    Ok(serving)
}
