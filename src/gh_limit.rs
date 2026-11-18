//! GitHub's rate-limit answers, told apart from "not signed in". When an account's REST API limit is used up, `gh`
//! exits non-zero with "API rate limit exceeded for user ID ..." (HTTP 403, or 429 for the secondary limit), which
//! looks like a failed sign-in to a caller that only checks the exit code. This reads that answer, and when the limit
//! resets (`X-RateLimit-Reset` or `Retry-After` when the output has headers, else `gh api rate_limit`, which GitHub
//! doesn't count against the limit).
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;

static LIMITED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)api rate limit exceeded|secondary rate limit|x-ratelimit-remaining:\s*0\b|abuse detection")
        .unwrap()
});
static RESET: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)x-ratelimit-reset:\s*(\d+)").unwrap());
static RETRY_AFTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)retry-after:\s*(\d+)").unwrap());

/// Whether what `gh` said (both streams, joined) is GitHub refusing for a used-up limit.
pub fn is_limited(said: &str) -> bool {
    LIMITED.is_match(said)
}

/// When the limit resets, as Unix seconds, if the output says.
pub fn reset_in(said: &str, now: u64) -> Option<u64> {
    if let Some(c) = RESET.captures(said) {
        return c[1].parse().ok();
    }
    RETRY_AFTER
        .captures(said)
        .and_then(|c| c[1].parse::<u64>().ok())
        .map(|s| now + s)
}

/// Unix seconds now.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `gh api rate_limit`'s core reset time, which costs no quota. `None` when it can't be asked or answers nothing.
pub async fn ask_reset(gh: &std::path::Path) -> Option<u64> {
    let args = ["api", "rate_limit", "--jq", ".resources.core.reset"];
    crate::host_call::guard_program("gh", None, &args).ok()?;
    let child = crate::spawn::tokio_command(gh)
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(Duration::from_secs(20), child).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .ok()
        .filter(|&n| n > 0)
}

/// What to tell a person: the limit is used up, and when it resets when known (UTC, HH:MM).
pub fn message(reset: Option<u64>) -> String {
    let at = reset
        .and_then(|s| chrono::DateTime::from_timestamp(i64::try_from(s).ok()?, 0))
        .map(|t| t.format("%H:%M UTC").to_string());
    match at {
        Some(at) => format!("GitHub's API limit for this account is used up; it resets at {at}"),
        None => "GitHub's API limit for this account is used up; it resets within the hour".to_string(),
    }
}

/// How long until `reset`, plus a few seconds of slack, if it is still ahead.
pub fn until(reset: Option<u64>, now: u64) -> Option<Duration> {
    reset.filter(|&r| r > now).map(|r| Duration::from_secs(r - now + 5))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_limit_answers_and_nothing_else() {
        assert!(is_limited(
            "gh: API rate limit exceeded for user ID 42. If you reach out (HTTP 403)"
        ));
        assert!(is_limited("You have exceeded a secondary rate limit. (HTTP 429)"));
        assert!(is_limited("HTTP/2.0 403\nX-Ratelimit-Remaining: 0\n"));
        assert!(!is_limited("X-Ratelimit-Remaining: 4999"));
        assert!(!is_limited(
            "You are not logged into any GitHub hosts. Run gh auth login"
        ));
        assert!(!is_limited("HTTP 401: Bad credentials"));
    }

    #[test]
    fn the_reset_comes_from_headers() {
        assert_eq!(reset_in("X-Ratelimit-Reset: 1790000000\n", 5), Some(1790000000));
        assert_eq!(reset_in("Retry-After: 60", 1000), Some(1060));
        assert_eq!(reset_in("nothing", 1000), None);
    }

    #[test]
    fn the_message_names_the_reset_time() {
        // 2026-11-18 16:44:00 UTC
        let t = chrono::DateTime::parse_from_rfc3339("2026-11-18T16:44:00Z")
            .unwrap()
            .timestamp() as u64;
        assert_eq!(
            message(Some(t)),
            "GitHub's API limit for this account is used up; it resets at 16:44 UTC"
        );
        assert!(message(None).contains("used up"));
        assert_eq!(until(Some(100), 40), Some(Duration::from_secs(65)));
        assert_eq!(until(Some(30), 40), None);
    }
}
