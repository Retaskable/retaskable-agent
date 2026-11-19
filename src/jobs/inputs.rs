//! What the computer's own settings give a job: the `scan` section as [`ScanSettings`], the Sample's screens for the
//! sweep, and the staging test token (`stagingToken` in `scanChore.ts`). Nothing here comes from the task but its url.
use super::a11y::A11ySettings;
use super::probes::has_scan_token;
use super::scan::ScanSettings;
use super::{Can, ChromeFlag, job_can};
use crate::config::ProjectConfig;
use crate::env::Env;

/// `SAMPLE_SCREENS` (app/scripts/sampleRoutes.ts): the app's `ROUTE_TABS` in the bar's order, without `start`, which
/// only a cloud source renders. The sweep opens each as `/<screen>?sample` on retaskable.com's own site.
pub const SAMPLE_SCREENS: [&str; 19] = [
    "board",
    "ideas",
    "bugs",
    "needs",
    "build",
    "checking",
    "review",
    "live",
    "whiteboard",
    "agents",
    "release",
    "flags",
    "design",
    "releases",
    "numbers",
    "requirements",
    "expert",
    "profiles",
    "settings",
];

/// The `scan` section with the deploy urls and `lighthouse.pages` it reads from the rest of the config.
pub fn scan_settings(cfg: &ProjectConfig) -> ScanSettings {
    ScanSettings::from_config(
        cfg,
        cfg.scan.probes,
        cfg.scan.a11y,
        cfg.scan.pages.clone(),
        cfg.scan.signed_in_paths.clone(),
    )
}

pub fn a11y_settings(cfg: &ProjectConfig) -> A11ySettings {
    A11ySettings {
        scan: scan_settings(cfg),
        sample_screens: SAMPLE_SCREENS.iter().map(std::string::ToString::to_string).collect(),
    }
}

/// The name a staging origin's test token is saved under, as a secret on the settings page or `retaskable secret set`:
/// `SCAN_TOKEN_` and the origin's host (and its port when it has one), upper-cased, anything but a letter or a digit
/// an underscore: `https://staging.retaskable.com` is `SCAN_TOKEN_STAGING_RETASKABLE_COM`. One name per origin keeps
/// the token to the site it is for (compliance-scan.md section 2: sent only to that origin). `None` for a url with
/// no host.
pub fn token_name(origin: &str) -> Option<String> {
    let url = reqwest::Url::parse(origin).ok()?;
    let host = url.host_str()?;
    let port = url.port().map(|p| format!("_{p}")).unwrap_or_default();
    let name: String = format!("SCAN_TOKEN_{host}{port}")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    Some(name)
}

/// The staging origin's saved test token, read now (a secret saved on the page is in the environment already, and
/// wins over one the environment had: `secrets::Secrets::apply`), or `None`. Never logged, never in a result.
pub fn staging_token(env: &Env, origin: &str) -> Option<String> {
    let value = env.get(&token_name(origin)?)?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// What this computer reports it `can` do for a project (`jobCan`): a checkout and an engine when the project has a
/// folder here, Chrome until a run found none, `scanToken` when the probes are on and the origin has a token, and
/// `issues:<tracker>` when it can read and write that tracker's issues: the project is on GitHub (the agent's own `gh`), or a
/// Linear or Jira key is saved here; `changes` when the project is on GitHub with a checkout here. The key itself is never sent.
pub fn can_for(cfg: &ProjectConfig, chrome: &ChromeFlag, env: &Env) -> Vec<Can> {
    use crate::issues::keys::{EnvKeys, Keyed, Keys};
    let token = has_scan_token(&scan_settings(cfg), &|origin| staging_token(env, origin));
    let mut can = job_can(!cfg.repo_path.trim().is_empty(), chrome, token);
    let keys = EnvKeys(env.clone());
    if crate::main_ci::reads(cfg) {
        can.push(Can::IssuesGithub);
    }
    if keys.get(Keyed::Linear).is_some() {
        can.push(Can::IssuesLinear);
    }
    if keys.get(Keyed::Jira).is_some() {
        can.push(Can::IssuesJira);
    }
    if !cfg.repo_path.trim().is_empty() && crate::main_ci::reads(cfg) {
        can.push(Can::Changes);
    }
    can.sort();
    can
}

#[cfg(test)]
mod tests;
