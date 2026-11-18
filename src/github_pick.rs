//! The owners and repos `gh` lists for **Link project…** on retaskable.com (`github_owners`, `github_repos`; organisations.md
//! section 6; `GithubPicker` in `agent/src/github.ts`): your login and your GitHub organisations, and one owner's repos,
//! newest push first with whether each is linked here already. `gh` runs with an argument list, never through a shell;
//! its answers are kept for 5 minutes, and `fresh` asks again. A failure is never kept. Owners and repos are checked
//! here: only an owner it listed is asked about. (The settings page's own picker is [`crate::repo_picker`], which keeps
//! 60 seconds and lists names only.)
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{Value, json};

/// How long an answer is kept (`KEEP_MS`).
pub const KEEP: Duration = Duration::from_secs(5 * 60);
const NOT_SIGNED_IN: &str = "gh isn't signed in: run `gh auth login`";

static NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]+$").unwrap());
static SCOPE_HINT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)read:org|scope|HTTP 403").unwrap());
static SIGNED_OUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)auth login|not logged in|authentication|HTTP 401").unwrap());

/// What `gh` answered: its exit code and both streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `gh` with these arguments; `Err` when it couldn't be started.
pub type Runner = Arc<dyn Fn(&[&str]) -> Result<Out, String> + Send + Sync>;
type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// The real `gh`.
fn real_gh(args: &[&str]) -> Result<Out, String> {
    crate::host_call::guard_program("gh", None, args).map_err(|e| e.to_string())?;
    let out = crate::spawn::command("gh")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("Couldn't run gh: {e}"))?;
    Ok(Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// The first line of what `gh` said, or its exit code (`firstLine`).
fn first_line(out: &Out) -> String {
    let text = if out.stderr.is_empty() {
        &out.stdout
    } else {
        &out.stderr
    };
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map_or_else(|| format!("exit code {}", out.code), str::to_string)
}

/// `gh api user`'s answer as the login, or why there is none (`parseProfile`).
fn login_of(out: &Out) -> Result<String, String> {
    if out.code != 0 {
        let said = format!("{}\n{}", out.stderr, out.stdout);
        if crate::gh_limit::is_limited(&said) {
            return Err(crate::gh_limit::message(crate::gh_limit::reset_in(
                &said,
                crate::gh_limit::now(),
            )));
        }
        let first = (if out.stderr.is_empty() {
            &out.stdout
        } else {
            &out.stderr
        })
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string();
        return Err(if first.is_empty() || SIGNED_OUT.is_match(&first) {
            NOT_SIGNED_IN.to_string()
        } else {
            first
        });
    }
    let user: Value =
        serde_json::from_str(&out.stdout).map_err(|_| "gh api user answered something that isn't JSON")?;
    match user.get("login").and_then(Value::as_str).filter(|l| !l.is_empty()) {
        Some(login) => Ok(login.to_string()),
        None => Err("gh api user answered without a login".to_string()),
    }
}

/// One repo as listed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    name_with_owner: String,
    description: Option<String>,
    private: bool,
    pushed_at: Option<String>,
    fork: bool,
}

/// `gh repo list --json`'s answer as rows, newest push first (`parseRepos`, less `linked`).
fn parse_repos(stdout: &str) -> Result<Vec<Row>, String> {
    let rows: Vec<Value> =
        serde_json::from_str(stdout).map_err(|e| format!("gh repo list answered something that isn't JSON: {e}"))?;
    let mut rows: Vec<Row> = rows
        .iter()
        .filter_map(|r| {
            Some(Row {
                name_with_owner: r.get("nameWithOwner")?.as_str()?.to_string(),
                description: r
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
                    .map(str::to_string),
                private: r.get("visibility").and_then(Value::as_str) != Some("PUBLIC"),
                pushed_at: r.get("pushedAt").and_then(Value::as_str).map(str::to_string),
                fork: r.get("isFork").and_then(Value::as_bool) == Some(true),
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        b.pushed_at
            .as_deref()
            .unwrap_or("")
            .cmp(a.pushed_at.as_deref().unwrap_or(""))
    });
    Ok(rows)
}

#[derive(Default)]
struct Cache {
    owners: Option<(Instant, Owners)>,
    /// By the lower-cased owner.
    repos: HashMap<String, (Instant, Vec<Row>)>,
}

/// Your login and your GitHub organisations, or why `gh` can't say.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Owners {
    login: Option<String>,
    orgs: Vec<String>,
    error: Option<String>,
    scope_missing: bool,
}

impl Owners {
    fn json(&self) -> Value {
        let mut v = json!({ "login": self.login, "orgs": self.orgs });
        if let Some(e) = &self.error {
            v["error"] = json!(e);
        }
        if self.scope_missing {
            v["scopeMissing"] = json!(true);
        }
        v
    }
}

/// The picker: `gh`'s answers kept for [`KEEP`].
pub struct GithubPicker {
    run: Runner,
    now: Clock,
    cache: Mutex<Cache>,
}

impl std::fmt::Debug for GithubPicker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GithubPicker")
    }
}

impl PartialEq for GithubPicker {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Default for GithubPicker {
    fn default() -> Self {
        Self::with(Arc::new(real_gh), Arc::new(Instant::now))
    }
}

impl GithubPicker {
    pub fn with(run: Runner, now: Clock) -> Self {
        Self {
            run,
            now,
            cache: Mutex::new(Cache::default()),
        }
    }

    fn cache(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn ask_owners(&self) -> Owners {
        let failed = |error: String| Owners {
            login: None,
            orgs: Vec::new(),
            error: Some(error),
            scope_missing: false,
        };
        let user = match (self.run)(&["api", "user"]) {
            Ok(out) => login_of(&out),
            Err(e) => Err(e),
        };
        let login = match user {
            Ok(login) => login,
            Err(e) => return failed(e),
        };
        let orgs = (self.run)(&[
            "api",
            "--paginate",
            "user/memberships/orgs?state=active",
            "--jq",
            ".[].organization.login",
        ]);
        let out = match orgs {
            Ok(out) => out,
            Err(e) => {
                return Owners {
                    login: Some(login),
                    orgs: Vec::new(),
                    error: Some(e),
                    scope_missing: false,
                };
            }
        };
        if out.code != 0 {
            let scope = SCOPE_HINT.is_match(&out.stderr);
            return Owners {
                login: Some(login),
                orgs: Vec::new(),
                error: (!scope).then(|| first_line(&out)),
                scope_missing: scope,
            };
        }
        let mut orgs: Vec<String> = Vec::new();
        for line in out.stdout.lines().map(str::trim).filter(|l| NAME.is_match(l)) {
            if !orgs.iter().any(|o| o == line) {
                orgs.push(line.to_string());
            }
        }
        orgs.sort_by_key(|o| o.to_lowercase());
        Owners {
            login: Some(login),
            orgs,
            error: None,
            scope_missing: false,
        }
    }

    fn owners_of(&self, fresh: bool) -> Owners {
        if fresh {
            self.cache().repos.clear();
        }
        let now = (self.now)();
        if !fresh
            && let Some((at, owners)) = &self.cache().owners
            && now.saturating_duration_since(*at) < KEEP
        {
            return owners.clone();
        }
        let owners = self.ask_owners();
        // A failure isn't kept.
        self.cache().owners = owners.login.is_some().then(|| (now, owners.clone()));
        owners
    }

    /// `github_owners { fresh? }`: your login and your GitHub organisations; `fresh` asks `gh` again (and forgets the
    /// repo lists).
    pub fn owners(&self, fresh: bool) -> Value {
        self.owners_of(fresh).json()
    }

    /// `github_repos { owner, fresh? }`: `owner`'s repos, refused for an owner [`owners`](Self::owners) didn't list.
    /// `linked` are the `owner/name`s linked on this machine already.
    pub fn repos(&self, owner: &str, fresh: bool, linked: &[String]) -> Result<Value, String> {
        let owners = self.owners_of(false);
        let canonical = owners
            .login
            .iter()
            .chain(&owners.orgs)
            .find(|n| n.eq_ignore_ascii_case(owner))
            .cloned()
            .ok_or_else(|| format!("{owner} isn't you or one of your GitHub organisations."))?;
        let key = canonical.to_lowercase();
        let now = (self.now)();
        let kept = self
            .cache()
            .repos
            .get(&key)
            .filter(|(at, _)| !fresh && now.saturating_duration_since(*at) < KEEP)
            .map(|(_, rows)| rows.clone());
        let rows = match kept {
            Some(rows) => rows,
            None => {
                let out = (self.run)(&[
                    "repo",
                    "list",
                    &canonical,
                    "--limit",
                    "1000",
                    "--no-archived",
                    "--json",
                    "nameWithOwner,description,visibility,pushedAt,isFork",
                ])?;
                if out.code != 0 {
                    return Err(format!("Couldn't list {canonical}'s repos: {}", first_line(&out)));
                }
                let rows = parse_repos(&out.stdout)?;
                self.cache().repos.insert(key, (now, rows.clone()));
                rows
            }
        };
        let linked: Vec<String> = linked.iter().map(|l| l.to_lowercase()).collect();
        Ok(Value::Array(
            rows.iter()
                .map(|r| {
                    json!({
                        "nameWithOwner": r.name_with_owner,
                        "description": r.description,
                        "private": r.private,
                        "pushedAt": r.pushed_at,
                        "fork": r.fork,
                        "linked": linked.contains(&r.name_with_owner.to_lowercase()),
                    })
                })
                .collect(),
        ))
    }
}

#[cfg(test)]
mod tests;
