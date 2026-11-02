//! The settings page's sign-in: the one-time tray token or the network password, sessions, the
//! CSRF check, the wrong-try rate limit, and the Host and Origin checks every request goes
//! through (agent-settings.md section 4.3).
use std::collections::HashMap;
use std::net::IpAddr;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::Response;

use super::{SettingsServer, not_found_response};
use crate::password;
use crate::store::MachineStore;
use std::sync::Arc;

pub(super) const COOKIE_NAME: &str = "retaskable_settings_session";
pub(super) const SESSION_TTL_SECS: u64 = 8 * 60 * 60;
const FAILURES_BEFORE_A_WAIT: u32 = 5;
const FIRST_WAIT_SECS: u64 = 60;
const MAX_WAIT_SECS: u64 = 60 * 60;

#[derive(Clone)]
pub(super) struct Session {
    pub(super) csrf: String,
    expires_at: u64,
}

#[derive(Default)]
pub(super) struct FailureState {
    count: u32,
    window_start: u64,
    blocked_until: u64,
    wait_secs: u64,
}

#[derive(Default)]
pub(super) struct InnerState {
    /// A tray-minted token, good once before it expires (section 4.2). Nothing in this agent
    /// mints one yet — the tray's own "Settings…" link isn't ported — so this stays empty
    /// outside tests until it is.
    pub(super) tokens: HashMap<String, u64>,
    pub(super) sessions: HashMap<String, Session>,
    pub(super) failures: HashMap<IpAddr, FailureState>,
}

impl SettingsServer {
    /// A missing, wrong, used or old token is refused the same way (section 6): once checked,
    /// right or wrong, it's gone.
    pub(super) fn verify_token(&self, token: &str) -> bool {
        let now = (self.now)();
        let mut state = self.state.lock().unwrap();
        matches!(state.tokens.remove(token), Some(expires_at) if expires_at > now)
    }

    /// Matches a password against this computer's network-access hash, constant time
    /// (section 3.1), whichever set it: `RETASKABLE_NETWORK_PASSWORD` or `machine.json`. Only
    /// while network access is on (section 3.1: "Turning it off keeps the hash" so it can be
    /// turned back on, but doesn't go on signing in while it's off).
    pub(super) fn verify_password(&self, password_attempt: &str) -> bool {
        let access = match &self.env_network_access {
            Some(access) => access.clone(),
            None => {
                let Ok(machine) = MachineStore::load(&self.dir) else {
                    return false;
                };
                machine.network().unwrap_or_default()
            }
        };
        access.enabled
            && access
                .password_hash
                .as_deref()
                .is_some_and(|hash| password::verify_password(password_attempt, hash))
    }

    pub(super) fn create_session(&self) -> (String, String) {
        let cookie = random_hex(32);
        let csrf = random_hex(32);
        let expires_at = (self.now)() + SESSION_TTL_SECS;
        self.state.lock().unwrap().sessions.insert(
            cookie.clone(),
            Session {
                csrf: csrf.clone(),
                expires_at,
            },
        );
        (cookie, csrf)
    }

    pub(super) fn authenticate(&self, headers: &HeaderMap) -> Option<Session> {
        let cookie_header = headers.get(header::COOKIE)?.to_str().ok()?;
        let token = parse_cookie(cookie_header, COOKIE_NAME)?;
        let now = (self.now)();
        let mut state = self.state.lock().unwrap();
        let session = state.sessions.get(&token)?.clone();
        if session.expires_at <= now {
            state.sessions.remove(&token);
            return None;
        }
        Some(session)
    }

    /// `None` while `addr` may try again now; `Some(seconds)` while it must still wait
    /// (section 4.3: even a right password waits).
    pub(super) fn check_rate_limit(&self, addr: IpAddr) -> Option<u64> {
        let now = (self.now)();
        let state = self.state.lock().unwrap();
        state
            .failures
            .get(&addr)
            .filter(|f| now < f.blocked_until)
            .map(|f| f.blocked_until - now)
    }

    /// Only called when `check_rate_limit` just said `addr` may try again, so a failure here is
    /// never while already blocked (section 4.3: five wrong tries within a minute wait a minute,
    /// then two, then four, up to an hour).
    pub(super) fn record_failure(&self, addr: IpAddr) {
        let now = (self.now)();
        let mut state = self.state.lock().unwrap();
        let entry = state.failures.entry(addr).or_insert_with(|| FailureState {
            count: 0,
            window_start: now,
            blocked_until: 0,
            wait_secs: 0,
        });
        if now.saturating_sub(entry.window_start) > 60 {
            entry.count = 0;
            entry.window_start = now;
        }
        entry.count += 1;
        if entry.count >= FAILURES_BEFORE_A_WAIT {
            entry.wait_secs = if entry.wait_secs == 0 {
                FIRST_WAIT_SECS
            } else {
                (entry.wait_secs * 2).min(MAX_WAIT_SECS)
            };
            entry.blocked_until = now + entry.wait_secs;
            entry.count = 0;
            entry.window_start = now;
        }
    }

    pub(super) fn record_success(&self, addr: IpAddr) {
        self.state.lock().unwrap().failures.remove(&addr);
    }

    /// Its Host is this computer's own, at the port it answered on (section 4.3): no DNS
    /// rebinding. Network access can already make the port listen on every address (section
    /// 4.4), but this still only accepts the loopback names; widening it to the computer's own
    /// network addresses isn't ported yet (tracked outside this change).
    pub(super) fn host_is_allowed(&self, host_header: &str) -> bool {
        let (host, port) = split_host_port(host_header);
        if let Some(port) = port
            && port != self.port
        {
            return false;
        }
        matches!(
            host.to_ascii_lowercase().as_str(),
            "127.0.0.1" | "localhost" | "[::1]" | "::1"
        )
    }
}

/// Section 4.3, every request: Host is this computer's own, and a POST's Origin is its own.
/// Anything else answers the same 404 as a path this agent doesn't serve, so it gives nothing
/// away about which paths exist (agent/08-http-api.md: the port takes no command either way).
pub(super) async fn security_envelope(
    State(settings): State<Arc<SettingsServer>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(host) = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
    else {
        return not_found_response();
    };
    if !settings.host_is_allowed(&host) {
        return not_found_response();
    }
    if req.method() == Method::POST {
        let expected = format!("http://{host}");
        let origin_ok = req
            .headers()
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|origin| origin.eq_ignore_ascii_case(&expected));
        if !origin_ok {
            return not_found_response();
        }
    }
    next.run(req).await
}

pub(super) fn csrf_ok(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get("X-CSRF-Token")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|token| constant_time_eq(token, expected))
}

pub(super) fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

pub(super) fn parse_cookie(header: &str, name: &str) -> Option<String> {
    header.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

/// Splits a Host header into its hostname and, when one is given, its port. An IPv6 literal
/// (`[::1]` or `[::1]:4700`) keeps its brackets as part of the hostname, same as a browser sends.
pub(super) fn split_host_port(host_header: &str) -> (&str, Option<u16>) {
    if let Some(after_bracket) = host_header.strip_prefix('[') {
        return match after_bracket.find(']') {
            Some(end) => {
                let host = &host_header[..end + 2];
                let port = after_bracket[end + 1..].strip_prefix(':').and_then(|p| p.parse().ok());
                (host, port)
            }
            None => (host_header, None),
        };
    }
    match host_header.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => (host, port.parse().ok()),
        _ => (host_header, None),
    }
}

pub(super) fn random_hex(bytes_len: usize) -> String {
    (0..bytes_len)
        .map(|_| format!("{:02x}", rand::random::<u8>()))
        .collect()
}

#[cfg(test)]
mod tests;
