//! docs/specs/agent-ci.md section 3.2: what the host reads for a check before its containers start, with the
//! token the containers never see: the token for the fetch, the pull request's changed files and its card line
//! (`CI_PIPELINE_FILES`, `CI_CARD_LINE`), as `runner.mjs` reads them.
use std::process::Stdio;
use std::sync::OnceLock;

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::Value;

use super::report::Host;

/// A pull request's files are listed this many to a page, at most `PAGES` pages.
const PER_PAGE: usize = 100;
const PAGES: usize = 30;

impl Host {
    /// `gh auth token`: the login the fetch container clones and fetches with.
    pub fn token(&self) -> Result<String> {
        let out = std::process::Command::new(&self.program)
            .current_dir(&self.dir)
            .args(["auth", "token"])
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("starting {}", self.program.display()))?;
        let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !out.status.success() || token.is_empty() {
            bail!("{} auth token: no GitHub login to fetch with", self.program.display());
        }
        Ok(token)
    }

    /// The pull request's changed files as the JSON list `CI_PIPELINE_FILES` holds; `[]` when they can't be read.
    pub fn pr_files(&self, pr: u64) -> String {
        let mut names: Vec<String> = Vec::new();
        for page in 1..=PAGES {
            let endpoint = format!("repos/{}/pulls/{pr}/files?per_page={PER_PAGE}&page={page}", self.repo);
            let Ok(body) = self.api("GET", &endpoint, &[]) else {
                return "[]".into();
            };
            let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(&body) else {
                return "[]".into();
            };
            names.extend(
                rows.iter()
                    .filter_map(|r| r.get("filename")?.as_str().map(String::from)),
            );
            if rows.len() < PER_PAGE {
                break;
            }
        }
        serde_json::to_string(&names).unwrap_or_else(|_| "[]".into())
    }

    /// The card line (`Release flag: ...`) of a `retaskable/` pull request's body: `Some("")` is no card, and
    /// `None` is a body the host would not give, which the check says and passes on.
    pub fn card_line(&self, pr: u64, branch: &str) -> Option<String> {
        if pr == 0 || !branch.starts_with("retaskable/") {
            return Some(String::new());
        }
        let body = self.api("GET", &format!("repos/{}/pulls/{pr}", self.repo), &[]).ok()?;
        let v: Value = serde_json::from_str(&body).ok()?;
        let text = v.get("body")?.as_str()?.replace('\r', "");
        static CARD: OnceLock<Regex> = OnceLock::new();
        let card = CARD.get_or_init(|| Regex::new(r"(?m)^Release flag: .*$").expect("a fixed pattern"));
        Some(card.find(&text).map_or(String::new(), |m| m.as_str().to_string()))
    }
}

#[cfg(all(test, unix))]
mod tests;
