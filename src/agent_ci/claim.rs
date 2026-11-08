//! docs/specs/agent-ci.md section 3.4: several computers, one repo. A key is claimed by posting a pending status
//! naming this computer on the merged commit's head and reading the statuses back. The pure decision is
//! `decide`; `claim` and `refreshing` are the small async drivers. No store call: the statuses are the only
//! coordination.
use std::future::Future;
use std::time::Duration;

use super::report::{self, Host, State, Status};

/// A claim is posted again this often while the check runs.
pub const REFRESH: Duration = Duration::from_secs(5 * 60);
/// A claim not refreshed for this long is stale and may be taken over.
pub const STALE_MS: i64 = 15 * 60 * 1000;

/// What a computer does with a key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Some computer already finished it (success or failure): never run again.
    Finished,
    /// Another computer's live claim comes first.
    Lose,
    /// This computer's claim wins; `stale` are the computers whose stale claims it takes over.
    Run { stale: Vec<String> },
}

/// A status description carrying the check key: `<text> (<key>)`. The text is cut so the key always survives
/// the host's length limit.
pub fn describe(text: &str, key: &str) -> String {
    let tag = format!(" ({key})");
    let room = 140usize.saturating_sub(tag.chars().count());
    format!("{}{tag}", text.chars().take(room).collect::<String>())
}

/// The computer that wrote a status for `key`, from `agent-ci <computer>: ... (<key>)`.
fn computer_of<'a>(s: &'a Status, context: &str, key: &str) -> Option<&'a str> {
    if s.context != context || !s.description.ends_with(&format!(" ({key})")) {
        return None;
    }
    s.description
        .strip_prefix("agent-ci ")?
        .split_once(": ")
        .map(|(c, _)| c)
}

fn at_ms(s: &Status) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(&s.updated_at)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// The earliest and the latest post of one computer's current run of claims: a gap of `STALE_MS` or more ends a
/// run, so a computer that went stale and came back claims afresh.
fn claim_span(mut times: Vec<i64>) -> Option<(i64, i64)> {
    times.sort_unstable();
    let last = *times.last()?;
    let mut first = last;
    for t in times.into_iter().rev() {
        if first - t >= STALE_MS {
            break;
        }
        first = t;
    }
    Some((first, last))
}

/// Decides from the statuses read back. `me` counts as a claimant starting `now_ms` when it has posted none yet.
pub fn decide(statuses: &[Status], key: &str, me: &str, now_ms: i64) -> Verdict {
    decide_in(report::PR_CONTEXT, statuses, key, me, now_ms)
}

/// `decide` over the statuses of one context (a pull request's or a push's).
pub fn decide_in(context: &str, statuses: &[Status], key: &str, me: &str, now_ms: i64) -> Verdict {
    let mut pending: std::collections::BTreeMap<&str, Vec<i64>> = Default::default();
    for s in statuses {
        let Some(computer) = computer_of(s, context, key) else {
            continue;
        };
        match s.state.as_str() {
            "success" | "failure" => return Verdict::Finished,
            "pending" => pending.entry(computer).or_default().push(at_ms(s).unwrap_or(now_ms)),
            _ => {}
        }
    }
    let (mut live, mut stale) = (Vec::new(), Vec::new());
    for (computer, times) in pending {
        let Some((first, last)) = claim_span(times) else {
            continue;
        };
        if now_ms - last >= STALE_MS && computer != me {
            stale.push(computer.to_string());
        } else {
            live.push((first, computer));
        }
    }
    if !live.iter().any(|(_, c)| *c == me) {
        live.push((now_ms, me));
    }
    live.sort();
    if live[0].1 == me {
        Verdict::Run { stale }
    } else {
        Verdict::Lose
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> anyhow::Result<T> + Send + 'static) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(f).await?
}

/// Claims `key` (the pull request's head `sha`) for `computer`: reads the statuses, and unless the key is
/// finished or another computer's live claim comes first, posts the claim and reads back to see who won. A
/// host that can't be reached gives an error: the check doesn't run (fail closed).
pub async fn claim(
    host: &std::sync::Arc<Host>,
    context: &'static str,
    sha: &str,
    key: &str,
    computer: &str,
    now_ms: impl Fn() -> i64,
) -> anyhow::Result<Verdict> {
    let read = || {
        let (h, sha) = (host.clone(), sha.to_string());
        blocking(move || h.read_statuses(&sha))
    };
    let first = decide_in(context, &read().await?, key, computer, now_ms());
    if !matches!(first, Verdict::Run { .. }) {
        return Ok(first);
    }
    let (h, sha_s) = (host.clone(), sha.to_string());
    let text = describe(&report::running_text(&report::me(computer)), key);
    blocking(move || h.post_status(&sha_s, context, State::Pending, &text, None)).await?;
    Ok(decide_in(context, &read().await?, key, computer, now_ms()))
}

/// Runs `work`, calling `post` every `every` meanwhile (the claim's refresh). Returns `work`'s output.
pub async fn refreshing<T, P, Fut>(every: Duration, post: P, work: impl Future<Output = T>) -> T
where
    P: Fn() -> Fut,
    Fut: Future<Output = ()>,
{
    tokio::pin!(work);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + every, every);
    loop {
        tokio::select! {
            out = &mut work => return out,
            _ = tick.tick() => post().await,
        }
    }
}

#[cfg(test)]
mod tests;
