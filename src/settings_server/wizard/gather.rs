//! Reading the facts the steps are built from (docs/specs/setup-wizard.md section 3.3): files on this computer, the
//! secrets store's states (names and fingerprints, never values), the links the page started, the programs panel's row
//! for the CI runner, and the two slow answers (GitHub, Claude) from the probes, kept for a few minutes.
use serde_json::Value;

use super::steps::{
    Approve, CiFacts, ClaudeFacts, Facts, GhOk, GithubFacts, LinkFacts, ProjectFacts, ProjectRow, Token,
    WaitingCommands, missing_scopes, parse_gh_user,
};
use super::{fresh, keep};
use crate::link;
use crate::paths;
use crate::program_settings::ci as ci_settings;
use crate::secrets::{SecretState, Source};
use crate::settings_server::SettingsServer;

pub(super) const CLAUDE_SECRETS: [&str; 2] = ["CLAUDE_CODE_OAUTH_TOKEN", "ANTHROPIC_API_KEY"];

fn token_of(states: &[SecretState], name: &str) -> (Token, String) {
    match states.iter().find(|s| s.name == name) {
        Some(s) => (
            match s.source {
                Source::Saved => Token::Saved,
                Source::Env => Token::Env,
                Source::Unreadable => Token::Unreadable,
                Source::Unset => Token::Unset,
            },
            s.fingerprint.clone().unwrap_or_default(),
        ),
        None => (Token::Unset, String::new()),
    }
}

impl SettingsServer {
    /// Why nothing can be saved on this computer, in words, or `None`: no key to seal with, or a data folder this
    /// program can't write (a Docker volume made by something else first is the usual cause).
    pub(super) fn storage_problem(&self) -> Option<String> {
        if let Some(why) = self.secrets.get().and_then(|s| s.save_problem()) {
            return Some(why);
        }
        let probe = self.dir.join(".write-test");
        let wrote = std::fs::create_dir_all(&self.dir)
            .and_then(|()| std::fs::write(&probe, b"ok"))
            .and_then(|()| std::fs::remove_file(&probe));
        wrote.err().map(|e| {
            format!(
                "This program can't write to its data folder ({}): {e}. In Docker the volume may have been made by \
                 something else first: stop everything and remove it (docker volume rm retaskable-data), then start again.",
                self.dir.display()
            )
        })
    }

    pub(super) async fn github_facts(&self, states: &[SecretState], force: bool) -> GithubFacts {
        let (token, about) = token_of(states, "GH_TOKEN");
        if token == Token::Unreadable {
            return GithubFacts { token, check: None };
        }
        if !force && let Some(check) = fresh(&self.wizard.github, &about) {
            return GithubFacts { token, check };
        }
        let check = self.check_github(token != Token::Unset).await;
        keep(&self.wizard.github, &about, check.clone());
        GithubFacts { token, check }
    }

    /// Asks GitHub who the token is, what it may do and whether it reaches repos. With no token, a `gh` that isn't
    /// signed in is "nothing yet" (`None`), not a failure.
    async fn check_github(&self, have_token: bool) -> Option<Result<GhOk, String>> {
        let probes = self.probes();
        let output = match probes.gh_user().await {
            Ok(text) => text,
            Err(why) => return have_token.then_some(Err(why)),
        };
        let (user, scopes) = match parse_gh_user(&output) {
            Ok(parsed) => parsed,
            Err(why) => return Some(Err(why)),
        };
        if let Some(why) = scopes.as_deref().and_then(missing_scopes) {
            return Some(Err(why));
        }
        if let Err(why) = probes.gh_repos().await {
            return Some(Err(why));
        }
        let identity = probes.identity(user.clone());
        Some(Ok(GhOk {
            login: user["login"].as_str().unwrap_or_default().to_string(),
            name: user["name"].as_str().filter(|n| !n.is_empty()).map(str::to_string),
            identity,
        }))
    }

    pub(super) async fn claude_facts(&self, states: &[SecretState], force: bool) -> ClaudeFacts {
        let saved = CLAUDE_SECRETS
            .into_iter()
            .find(|n| matches!(token_of(states, n).0, Token::Saved | Token::Env));
        let about = CLAUDE_SECRETS.map(|n| token_of(states, n).1).join(",");
        let status = match (!force).then(|| fresh(&self.wizard.claude, &about)).flatten() {
            Some(status) => status,
            None => {
                let status = self.probes().claude().await;
                keep(&self.wizard.claude, &about, status.clone());
                status
            }
        };
        ClaudeFacts { saved, status }
    }

    fn project_facts(&self) -> ProjectFacts {
        let Some(add) = self.add_project.get() else {
            return ProjectFacts::default();
        };
        let mut facts = ProjectFacts {
            on_computer: add
                .projects()
                .into_iter()
                .map(|(id, repo)| ProjectRow { id, repo })
                .collect(),
            ..ProjectFacts::default()
        };
        match add.status() {
            crate::add_project::Status::Working { repo, step } => facts.adding = Some((repo, step)),
            crate::add_project::Status::Failed { repo, message } => facts.add_failed = Some((repo, message)),
            _ => {}
        }
        facts
    }

    fn waiting_commands(&self, projects: &ProjectFacts) -> Vec<WaitingCommands> {
        let Some(view) = self.agent_view() else {
            return Vec::new();
        };
        projects
            .on_computer
            .iter()
            .filter_map(|p| {
                let w = view.item_commands.gates().waiting(&p.id)?;
                Some(WaitingCommands {
                    project: p.repo.clone(),
                    text: w.text,
                    by: w.by,
                })
            })
            .collect()
    }

    /// Step 1's facts and, for step 6, the runner's row of the programs panel.
    fn link_and_ci(&self, can_act: bool, projects: &ProjectFacts, github_ok: bool) -> (LinkFacts, CiFacts) {
        let programs = self.programs_view(can_act);
        let row = programs["programs"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["id"] == "ci-runner"))
            .cloned()
            .unwrap_or(Value::Null);
        let cfg = crate::config::load(&self.dir).unwrap_or_default();
        let cloud_url = link::cloud_url(&cfg, &self.env, None).unwrap_or_default();
        let store_url = crate::backlog::url(&self.env);
        let held = |url: &str| !url.is_empty() && matches!(self.keychain.get(url), Ok(Some(_)));

        let state = row["state"].as_str().unwrap_or("notInstalled").to_string();
        let off = ci_settings::read(&paths::ci_dir(&self.dir)).is_ok_and(|f| f.slots == Some(0));
        let runner_linked = row["link"]["linked"] == true;
        let mut links = LinkFacts {
            cloud_name: link::environment_name(&cloud_url).to_string(),
            cloud_url: cloud_url.clone(),
            cloud: held(&cloud_url),
            store: held(&store_url),
            runner: (!off && state != "notInstalled").then_some(runner_linked),
            ..LinkFacts::default()
        };
        if let Some(l) = self.links.get() {
            links.working = !l.start_allowed(&cloud_url) || !l.start_allowed(&store_url);
            let waiting = |label: &str, url: &str, code: &str| Approve {
                label: label.to_string(),
                url: link::pending::web_address(url),
                code: code.to_string(),
            };
            if let Some(w) = l.waiting(&cloud_url) {
                links.waiting = Some(waiting("retaskable.com", &w.page_url, &w.user_code));
            } else if let Some(w) = l.backlog() {
                links.waiting = Some(Approve {
                    label: "the shared backlog".into(),
                    url: link::pending::web_address(&w.page_url),
                    code: String::new(),
                });
            }
            for url in [&cloud_url, &store_url] {
                if !held(url)
                    && let Some(link::pending::Outcome::Failed(why)) = l.outcome(url)
                {
                    links.error = Some(why);
                }
            }
        }
        if links.waiting.is_none() && row["link"]["working"] == true {
            links.working = true;
            links.waiting = Some(Approve {
                label: "the CI runner".into(),
                url: row["link"]["approve"]["url"].as_str().map(str::to_string),
                code: row["link"]["approve"]["code"].as_str().unwrap_or_default().to_string(),
            });
        }
        if links.error.is_none() {
            links.error = row["link"]["error"].as_str().map(str::to_string);
        }

        let secret = row["secrets"]
            .as_array()
            .and_then(|rows| rows.iter().find(|r| r["name"] == "GH_TOKEN"))
            .cloned()
            .unwrap_or(Value::Null);
        let token = match (secret["source"].as_str(), secret["inherited"] == true) {
            (Some("saved"), true) => Some("the code agent's token".to_string()),
            (Some("saved"), false) => Some("its own saved token".to_string()),
            (Some("env"), _) => Some("its environment".to_string()),
            // Run natively, `gh` has its own login the runner shares.
            _ if github_ok && !crate::page_host::docker_trust::enabled(&self.env) => Some("gh's own login".to_string()),
            _ => None,
        };
        let ci = CiFacts {
            off,
            linked: runner_linked,
            token,
            docker: row["details"]["docker"].as_str().map(str::to_string),
            last_error: row["lastError"].as_str().map(str::to_string),
            last_check: last_check(&paths::ci_dir(&self.dir), (self.now)() * 1000),
            repos: projects.on_computer.iter().map(|p| p.repo.clone()).collect(),
            state,
        };
        (links, ci)
    }

    /// Everything the steps read. `force` asks GitHub and Claude again instead of using a kept answer.
    pub(super) async fn wizard_facts(&self, can_act: bool, force: bool) -> Facts {
        let states = self.secrets.get().map(|s| s.states()).unwrap_or_default();
        let github = self.github_facts(&states, force).await;
        let claude = self.claude_facts(&states, force).await;
        let projects = self.project_facts();
        let waiting = self.waiting_commands(&projects);
        let github_ok = matches!(github.check, Some(Ok(_)));
        let (links, ci) = self.link_and_ci(can_act, &projects, github_ok);
        Facts {
            computer: link::machine_name(&self.env),
            can_act,
            storage_problem: self.storage_problem(),
            links,
            github,
            claude,
            projects,
            waiting,
            ci,
        }
    }

    /// The target the page posts to `/api/link/{target}` for this computer's own cloud.
    pub(super) fn link_target(&self) -> String {
        let cfg = crate::config::load(&self.dir).unwrap_or_default();
        let url = link::cloud_url(&cfg, &self.env, None).unwrap_or_default();
        link::environment_name(&url).to_lowercase()
    }
}

/// "PR #12 on o/r passed in 40 s, 3 minutes ago" from the runner's `last-check.json`, or `None`.
fn last_check(ci_dir: &std::path::Path, now_ms: u64) -> Option<String> {
    let text = std::fs::read_to_string(ci_dir.join("last-check.json")).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    let ago = now_ms.saturating_sub(v["at"].as_u64()?) / 60_000;
    let when = match ago {
        0 => "just now".to_string(),
        1 => "1 minute ago".to_string(),
        m if m < 120 => format!("{m} minutes ago"),
        m => format!("{} hours ago", m / 60),
    };
    Some(format!(
        "Last check: {} on {} {} in {} s, {when}.",
        v["subject"].as_str()?,
        v["repo"].as_str()?,
        if v["passed"] == true { "passed" } else { "failed" },
        v["ms"].as_u64().unwrap_or(0) / 1000
    ))
}

#[cfg(test)]
pub(super) fn last_check_for_test(dir: &std::path::Path, now_ms: u64) -> Option<String> {
    last_check(dir, now_ms)
}
