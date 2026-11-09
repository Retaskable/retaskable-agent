//! The settings page's sign-in: the one-time tray token or the network password, sessions, the
//! CSRF check, the wrong-try rate limit, and the Host and Origin checks every request goes
//! through (agent-settings.md section 4.3).
use std::collections::HashMap;
use std::net::IpAddr;

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::Response;

use super::{SettingsServer, not_found_response};
use crate::password;
use crate::store::MachineStore;
use std::sync::Arc;

pub(super) const COOKIE_NAME: &str = "retaskable_settings_session";
pub(super) const SESSION_TTL_SECS: u64 = 8 * 60 * 60;
/// How long a sign-in token from `settings_link` stays good.
pub(super) const TOKEN_TTL_SECS: u64 = 5 * 60;
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
    /// A token made by [`SettingsServer::settings_link`], good once before it expires (section 4.2).
    pub(super) tokens: HashMap<String, u64>,
    pub(super) sessions: HashMap<String, Session>,
    pub(super) failures: HashMap<IpAddr, FailureState>,
}

impl SettingsServer {
    /// A new sign-in token, good once within [`TOKEN_TTL_SECS`] (section 4.2).
    fn mint_token(&self) -> String {
        let token = random_hex(32);
        let expires_at = (self.now)() + TOKEN_TTL_SECS;
        let mut state = self.state.lock().unwrap();
        state.tokens.retain(|_, expires| *expires > (self.now)());
        state.tokens.insert(token.clone(), expires_at);
        token
    }

    /// The address that opens the settings page signed in: `http://127.0.0.1:<port>/settings#<token>`,
    /// with a token made for this call (section 4.2). The fragment never reaches a log or a `Referer`.
    pub fn settings_link(&self) -> String {
        format!("http://127.0.0.1:{}/settings#{}", self.port, self.mint_token())
    }

    /// A missing, wrong, used or old token is refused the same way (section 6): once checked,
    /// right or wrong, it's gone. A token is taken only from this computer itself (section 4.2),
    /// so one from another address is refused without being used up.
    pub(super) fn verify_token(&self, token: &str, from: IpAddr) -> bool {
        if !from.is_loopback() {
            return false;
        }
        let now = (self.now)();
        let mut state = self.state.lock().unwrap();
        matches!(state.tokens.remove(token), Some(expires_at) if expires_at > now)
    }

    /// Matches a password against this computer's network-access hash, constant time
    /// (section 3.1), whichever set it: `RETASKABLE_NETWORK_PASSWORD` or `machine.json`. Only
    /// while network access is on (section 3.1: "Turning it off keeps the hash" so it can be
    /// turned back on, but doesn't go on signing in while it's off).
    pub(super) fn verify_password(&self, password_attempt: &str) -> bool {
        let Some(access) = self.live_network_access() else {
            return false;
        };
        access.enabled
            && access
                .password_hash
                .as_deref()
                .is_some_and(|hash| password::verify_password(password_attempt, hash))
    }

    /// Network access as it is right now: the variable's, else `machine.json`'s, read on each
    /// call so a save that turns it off takes effect at once (section 4.4). `None` when the file
    /// can't be read (fail closed).
    fn live_network_access(&self) -> Option<crate::network::NetworkAccess> {
        match &self.env_network_access {
            Some(access) => Some(access.clone()),
            None => Some(MachineStore::load(&self.dir).ok()?.network().unwrap_or_default()),
        }
    }

    /// Whether network access is on right now (not as the agent started).
    pub(super) fn network_on(&self) -> bool {
        self.live_network_access().is_some_and(|a| a.enabled)
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
    /// rebinding. `127.0.0.1`, `localhost` and `[::1]` always; while network access is on, also
    /// an IP literal that is one of this computer's own interface addresses, behind the
    /// `settings-page` flag (Off: loopback only, as before). Never another name.
    pub(super) fn host_is_allowed(&self, host_header: &str) -> bool {
        let (host, port) = split_host_port(host_header);
        if let Some(port) = port
            && port != self.port
        {
            return false;
        }
        let host = host.to_ascii_lowercase();
        if matches!(host.as_str(), "127.0.0.1" | "localhost" | "[::1]" | "::1") {
            return true;
        }
        let literal = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(&host);
        let Ok(ip) = literal.parse::<IpAddr>() else {
            return false;
        };
        self.settings_page_flag.load(std::sync::atomic::Ordering::Relaxed)
            && self.network_on()
            && (self.own_addresses)().contains(&ip)
    }
}

/// Section 4.3, every request: Host is this computer's own, and a POST's Origin is its own.
/// Anything else answers the same 404 as a path this agent doesn't serve, so it gives nothing
/// away about which paths exist (agent/08-http-api.md: the port takes no command either way).
pub(super) async fn security_envelope(
    State(settings): State<Arc<SettingsServer>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
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
    // Another computer gets in only while network access is on, read live (section 4.4).
    if (!peer.ip().is_loopback() && !settings.network_on()) || !settings.host_is_allowed(&host) {
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
/// (`[::1]` or `[::1]:4700`) keeps its brackets as part of the hostname, same as a browser sends. A port that isn't
/// a number up to 65535 leaves the whole header as the host, which no allowed name matches.
pub(super) fn split_host_port(host_header: &str) -> (&str, Option<u16>) {
    if let Some(after_bracket) = host_header.strip_prefix('[') {
        return match after_bracket.find(']') {
            Some(end) => {
                let host = &host_header[..end + 2];
                match after_bracket[end + 1..].strip_prefix(':') {
                    None => (host, None),
                    Some(p) => match p.parse() {
                        Ok(port) if p.chars().all(|c| c.is_ascii_digit()) => (host, Some(port)),
                        _ => (host_header, None),
                    },
                }
            }
            None => (host_header, None),
        };
    }
    match host_header.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => match port.parse() {
            Ok(port) => (host, Some(port)),
            // Too big for a port: not a Host this agent answers, rather than "no port" skipping the check.
            Err(_) => (host_header, None),
        },
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
