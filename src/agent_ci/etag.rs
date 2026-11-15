//! docs/specs/agent-ci.md section 3.3: GETs the loop repeats every poll (a head's statuses) with GitHub's validators,
//! a port of Retaskable.com's `.agent-ci/etagCache.mjs`. A 304 costs nothing against the login's 5000 calls an hour,
//! so a poll that finds nothing changed is free.
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result, bail};

use super::report::Host;

/// One answer: a 304's has no body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub status: u16,
    pub etag: Option<String>,
    pub body: String,
}

/// The last answer of each URL that came with an ETag.
#[derive(Default)]
pub struct Cache {
    seen: Mutex<HashMap<String, (String, Answer)>>,
}

impl Cache {
    /// A GET that sends the last answer's ETag and, on a 304, returns that answer again. `send` is one request:
    /// the URL and the ETag to send, if any.
    pub fn get(&self, url: &str, send: impl FnOnce(&str, Option<&str>) -> Result<Answer>) -> Result<Answer> {
        let before = self.seen.lock().unwrap().get(url).cloned();
        let r = send(url, before.as_ref().map(|(etag, _)| etag.as_str()))?;
        if r.status == 304 {
            // Not asked for with an ETag: nothing to give back.
            return Ok(before.map_or(r, |(_, answer)| answer));
        }
        if r.status == 200
            && let Some(etag) = &r.etag
        {
            self.seen
                .lock()
                .unwrap()
                .insert(url.to_string(), (etag.clone(), r.clone()));
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
    CACHE.get_or_init(Cache::default)
}

impl Host {
    /// `gh api <endpoint>` as a GET with the last answer's ETag, so an unchanged answer is a free 304.
    pub fn get_cached(&self, endpoint: &str) -> Result<String> {
        let key = format!("{}|{endpoint}", self.repo);
        let r = shared().get(&key, |_, etag| self.send(endpoint, etag))?;
        if !(200..300).contains(&r.status) {
            bail!("{} api {endpoint}: HTTP {}", self.program.display(), r.status);
        }
        Ok(r.body)
    }

    fn send(&self, endpoint: &str, etag: Option<&str>) -> Result<Answer> {
        let header = etag.map(|e| format!("if-none-match: {e}"));
        let mut args = vec!["api", "-X", "GET", endpoint, "-i"];
        if let Some(h) = &header {
            args.extend(["-H", h]);
        }
        crate::host_call::guard_program(&self.program, Some(&self.dir), &args)?;
        let out = std::process::Command::new(&self.program)
            .current_dir(&self.dir)
            .args(&args)
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("starting {}", self.program.display()))?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        // `gh` exits non-zero for a 304 and for errors, but still prints the status line first.
        if !out.status.success() && !stdout.starts_with("HTTP/") {
            bail!(
                "{} api {endpoint}: {}",
                self.program.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(parse_include(&stdout))
    }
}

#[cfg(test)]
mod tests;
