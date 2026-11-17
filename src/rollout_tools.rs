//! Whether this computer can run a project's rollout commands at all (`agent/src/rolloutTools.ts`, any-computer-releases.md
//! section 3.16): the commands' program is installed, and a rollout that talks to Cloudflare has the account id and API
//! token in the computer's environment. A computer missing either doesn't release (the release gate says why), so it
//! never takes the rollout's lease and stalls a release it can't run.
use std::path::{Path, PathBuf};

use crate::config::DeployConfig;
use crate::engines::claude::NO_DEPLOY_TOKEN;
use crate::env::Env;

fn set(s: &Option<String>) -> Option<&str> {
    s.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

/// The project's rollout, `(traffic, health)`, or `None` when there is none: a command, a branch or production's address
/// missing (`rolloutOf` in `shared/rolloutSetting.ts`). Staging's branch counts as set when merges go to production.
pub fn rollout_of(deploy: &DeployConfig) -> Option<(String, String)> {
    let production = deploy.production.as_ref()?;
    let rollout = production.rollout.as_ref()?;
    let (traffic, health) = (set(&rollout.traffic)?, set(&rollout.health)?);
    let staging_ok = crate::promote::merges_to_production(deploy)
        || deploy.staging.as_ref().is_some_and(|s| !s.branch.trim().is_empty());
    let address = production.url.as_deref().is_some_and(|u| !u.trim().is_empty());
    (staging_ok && !production.branch.trim().is_empty() && address).then(|| (traffic.to_string(), health.to_string()))
}

/// Whether `program` is on the PATH (or a path that exists), as a shell would find it.
pub fn on_path(program: &str, env: &Env) -> bool {
    if program.contains(['/', '\\']) {
        return Path::new(program).exists();
    }
    let exts: Vec<String> = if cfg!(windows) {
        let pathext = env.get("PATHEXT").unwrap_or_else(|| ".EXE;.CMD;.BAT".into());
        std::iter::once(String::new())
            .chain(pathext.split(';').map(str::to_string))
            .collect()
    } else {
        vec![String::new()]
    };
    let path = env.get("PATH").or_else(|| env.get("Path")).unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
        .any(|dir| {
            exts.iter()
                .any(|ext| PathBuf::from(&dir).join(format!("{program}{ext}")).exists())
        })
}

/// What a command that reaches Cloudflare looks like: the repo's own `rollout:traffic` and `rollout:health` scripts, or wrangler.
fn reaches_cloudflare(command: &str) -> bool {
    let word = |w: &str| {
        command.match_indices(w).any(|(i, _)| {
            let before = command[..i].chars().next_back();
            let after = command[i + w.len()..].chars().next();
            let edge = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
            edge(before) && edge(after)
        })
    };
    word("wrangler") || word("rollout:traffic") || word("rollout:health")
}

/// What stops this computer running the project's rollout commands, in words for the settings line, or `None` when
/// nothing does (or it has no rollout commands).
pub fn missing(deploy: &DeployConfig, env: &Env, has: &dyn Fn(&str) -> bool) -> Option<String> {
    let (traffic, health) = rollout_of(deploy)?;
    let commands = [traffic, health];
    for command in &commands {
        let program = command.split_whitespace().next().unwrap_or("");
        if !program.is_empty() && !has(program) {
            return Some(format!(
                "{program} isn't installed on this computer, so it can't run the rollout"
            ));
        }
    }
    if commands.iter().any(|c| reaches_cloudflare(c)) {
        let filled = |name: &str| env.get(name).is_some_and(|v| !v.trim().is_empty());
        if !filled("CLOUDFLARE_ACCOUNT_ID") {
            return Some("CLOUDFLARE_ACCOUNT_ID isn't set on this computer, so it can't run the rollout".into());
        }
        if !filled("CLOUDFLARE_API_TOKEN") || env.get("CLOUDFLARE_API_TOKEN").as_deref() == Some(NO_DEPLOY_TOKEN) {
            return Some("CLOUDFLARE_API_TOKEN isn't set on this computer, so it can't run the rollout".into());
        }
    }
    None
}

#[cfg(test)]
mod tests;
