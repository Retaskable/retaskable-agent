//! Shared by the code agent and the CI runner (docs/specs/programs.md: neutral). docs/specs/agent-ci.md section 3.3: GETs the loop repeats every poll (a head's statuses) with GitHub's validators,
//! a port of Retaskable.com's `.agent-ci/etagCache.mjs`. A 304 costs nothing against the login's 5000 calls an hour,
//! so a poll that finds nothing changed is free.
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use anyhow::{Context, Result, bail};

use crate::github_budget::{self, Budget, Level};

/// Answers kept at most: a head's statuses are one URL per commit, so the cache would only grow.
const KEPT_MAX: usize = 2000;

/// One answer: a 304's has no body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub etag: Option<String>,
    pub body: String,
}

/// The last answer of each URL that came with an ETag, and when it was got.
#[derive(Default)]
pub struct Cache {
    seen: Mutex<HashMap<String, (String, Answer, Instant)>>,
    /// The account's allowance, when this cache answers to it: while it is low an answer held is reused without a request, and
    /// while it is nearly spent nothing new is asked for.
    budget: Option<&'static Budget>,
}

impl Cache {
    pub fn with_budget(budget: &'static Budget) -> Self {
        Self {
            seen: Mutex::default(),
            budget: Some(budget),
        }
    }

    /// A GET that sends the last answer's ETag and, on a 304, returns that answer again. `send` is one request:
    /// the URL and the ETag to send, if any.
    pub fn get(&self, url: &str, send: impl FnOnce(&str, Option<&str>) -> Result<Answer>) -> Result<Answer> {
        let before = self.seen.lock().unwrap().get(url).cloned();
        if let Some(budget) = self.budget {
            if let Some((_, answer, at)) = &before
                && at.elapsed() < budget.reuse_for()
            {
                return Ok(answer.clone());
            }
            if before.is_none() && budget.level() == Level::Critical {
                bail!("the GitHub allowance is nearly spent: {url} waits for its reset");
            }
        }
        let r = send(url, before.as_ref().map(|(etag, _, _)| etag.as_str()))?;
        if r.status == 304 {
            // Not asked for with an ETag: nothing to give back.
            if let Some((etag, answer, _)) = before {
                self.seen
                    .lock()
                    .unwrap()
                    .insert(url.to_string(), (etag, answer.clone(), Instant::now()));
                return Ok(answer);
            }
            return Ok(r);
        }
        if r.status == 200
            && let Some(etag) = &r.etag
        {
            let mut seen = self.seen.lock().unwrap();
            if seen.len() >= KEPT_MAX {
                seen.clear();
            }
            seen.insert(url.to_string(), (etag.clone(), r.clone(), Instant::now()));
        }
        Ok(r)
    }
}

/// What `gh api -i` printed: the status line, the headers, a blank line and the body. Output that doesn't start
/// with a status line is the body of a 200 with no ETag.
pub fn parse_include(out: &str) -> Answer {
    let plain = || Answer {
        status: 200,
        etag: None,
        body: out.to_string(),
    };
    let Some(first) = out.lines().next().filter(|l| l.starts_with("HTTP/")) else {
        return plain();
    };
    let Some(status) = first.split_whitespace().nth(1).and_then(|s| s.parse().ok()) else {
        return plain();
    };
    let (head, body) = out
        .split_once("\r\n\r\n")
        .or_else(|| out.split_once("\n\n"))
        .unwrap_or((out, ""));
    let etag = head.lines().skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case("etag").then(|| v.trim().to_string())
    });
    Answer {
        status,
        etag,
        body: body.to_string(),
    }
}

fn shared() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(|| Cache::with_budget(github_budget::global()))
}

/// A request GitHub answered with something other than 2xx: unlike a `gh` that would not run, that is its answer.
#[derive(Debug)]
pub struct Status {
    what: String,
    pub status: u16,
    said: String,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: HTTP {}: {}", self.what, self.status, self.said)
    }
}

impl std::error::Error for Status {}

/// `<program> api <endpoint>` as a GET run in `dir`, with the last answer's ETag, so an unchanged answer is a free 304.
/// `repo` only keys the cache. The body of a 2xx answer; any other status is an error.
pub fn get_cached_in(program: &OsStr, dir: &Path, repo: &str, endpoint: &str) -> Result<String> {
    let key = format!("{repo}|{endpoint}");
    let r = shared().get(&key, |_, etag| send_in(program, dir, endpoint, etag))?;
    if !(200..300).contains(&r.status) {
        let said: String = r.body.chars().take(300).collect();
        return Err(Status {
            what: format!("{} api {endpoint}", Path::new(program).display()),
            status: r.status,
            said: said.trim().to_string(),
        }
        .into());
    }
    Ok(r.body)
}

fn send_in(program: &OsStr, dir: &Path, endpoint: &str, etag: Option<&str>) -> Result<Answer> {
    let header = etag.map(|e| format!("if-none-match: {e}"));
    let mut args = vec!["api", "-X", "GET", endpoint, "-i"];
    if let Some(h) = &header {
        args.extend(["-H", h]);
    }
    crate::host_call::guard_program(program, Some(dir), &args)?;
    let out = crate::spawn::command(program)
        .current_dir(dir)
        .args(&args)
        .stdin(Stdio::null())
        .output()
        .with_context(|| format!("starting {}", Path::new(program).display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    // `gh` exits non-zero for a 304 and for errors, but still prints the status line first.
    if !out.status.success() && !stdout.starts_with("HTTP/") {
        bail!(
            "{} api {endpoint}: {}",
            Path::new(program).display(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    if stdout.starts_with("HTTP/") {
        let head = stdout
            .split_once("\r\n\r\n")
            .or_else(|| stdout.split_once("\n\n"))
            .map_or(stdout.as_str(), |(h, _)| h);
        github_budget::global().record_head(head);
    }
    Ok(parse_include(&stdout))
}

#[cfg(test)]
mod tests;
