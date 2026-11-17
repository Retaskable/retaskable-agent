//! The security probes' checks (`CHECKS` in Retaskable.com's `agent/src/securityProbes.ts`): each reads what
//! [`super::probes::run_probes`] saw and gives one of shared/asvs.ts's statuses with a short detail for the
//! write-up. Pure, so each is tested with plain answers. The ids are shared/asvs.ts's `PROBES`, in its order.
use std::collections::HashMap;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::Regex;

use super::probes::{ProbeResponse, ProbeTarget, TlsAnswer, TlsVersion};

/// A check's result, lowest first (`ASVS_STATUSES`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Na,
    Met,
    Unknown,
    NotMet,
    Error,
}

/// One check's verdict: its status and what was seen.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub status: Status,
    pub detail: String,
}

fn met(detail: impl Into<String>) -> Verdict {
    Verdict {
        status: Status::Met,
        detail: detail.into(),
    }
}

fn not_met(detail: impl Into<String>) -> Verdict {
    Verdict {
        status: Status::NotMet,
        detail: detail.into(),
    }
}

fn unknown(detail: impl Into<String>) -> Verdict {
    Verdict {
        status: Status::Unknown,
        detail: detail.into(),
    }
}

fn error(detail: impl Into<String>) -> Verdict {
    Verdict {
        status: Status::Error,
        detail: detail.into(),
    }
}

/// What an answer is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Page,
    NotFound,
    Api,
    SecurityTxt,
}

/// One answer as the probes read it, with how the write-up names it: the page's path, "the 404", "the API answer",
/// "security.txt".
#[derive(Debug, Clone)]
pub struct Answer {
    pub resp: ProbeResponse,
    pub kind: Kind,
    pub label: String,
}

/// What the probes saw.
pub struct Seen<'a> {
    pub target: &'a ProbeTarget,
    pub answers: Vec<Answer>,
    /// The signed-in paths' answers, read with the test token.
    pub signed_in: Vec<Answer>,
    pub cors: Option<ProbeResponse>,
    pub preflight: Option<ProbeResponse>,
    pub navigated: Option<ProbeResponse>,
    pub plain: Option<ProbeResponse>,
    pub security_txt: Option<ProbeResponse>,
    pub current: TlsAnswer,
    pub old: Vec<(TlsVersion, TlsAnswer)>,
    pub now: DateTime<Utc>,
}

type Check = fn(&Seen) -> Verdict;

/// The API path retaskable.com answers signed out (401), the one `retaskable` checks read.
pub const API_PATH: &str = "/api/me";
/// The origin sent to see whether CORS echoes it.
pub const SCAN_ORIGIN: &str = "https://scan.invalid";
const YEAR_S: u64 = 31_536_000;
const CERT_MIN_DAYS: i64 = 14;
const ERROR_TEXT_MAX: usize = 200_000;

/// The checks, by probe, in shared/asvs.ts's order.
pub const CHECKS: [(&str, Check); 23] = [
    ("http-redirect", http_redirect),
    ("tls-versions", tls_versions),
    ("certificate", certificate),
    ("hsts", hsts),
    ("charset", charset),
    ("api-not-renderable", api_not_renderable),
    ("cors-origin", cors_origin),
    ("cookie-secure", cookie_secure),
    ("csp-strict", csp_strict),
    ("nosniff", nosniff),
    ("referrer-policy", referrer_policy),
    ("frame-ancestors", frame_ancestors),
    ("cookie-samesite", cookie_samesite),
    ("cookie-host-prefix", cookie_host_prefix),
    ("cookie-httponly", cookie_httponly),
    ("error-leak", error_leak),
    ("signed-in-no-store", signed_in_no_store),
    ("coop", coop),
    ("sri", sri),
    ("server-version", server_version),
    ("trusted-types", trusted_types),
    ("permissions-policy", permissions_policy),
    ("security-txt", security_txt),
];

fn rx(p: &str) -> Regex {
    Regex::new(p).unwrap()
}

fn header<'a>(r: &'a ProbeResponse, name: &str) -> &'a str {
    r.headers.get(name).map_or("", String::as_str)
}

fn labels(answers: &[&Answer]) -> String {
    answers.iter().map(|a| a.label.as_str()).collect::<Vec<_>>().join(", ")
}

/// The answers that aren't redirects: what "every answer" means.
fn served<'a>(s: &'a Seen) -> Vec<&'a Answer> {
    s.answers
        .iter()
        .filter(|a| a.resp.status < 300 || a.resp.status >= 400)
        .collect()
}

fn pages<'a>(s: &'a Seen) -> Vec<&'a Answer> {
    served(s).into_iter().filter(|a| a.kind == Kind::Page).collect()
}

static HTML: LazyLock<Regex> = LazyLock::new(|| rx(r"(?i)^text/html\b"));

fn is_html(r: &ProbeResponse) -> bool {
    HTML.is_match(header(r, "content-type"))
}

const NO_PAGE: &str = "No page of the staging site answered";

/// `every` answer passes `ok`: met, or not met naming the ones that don't; unknown when nothing answered.
fn on_every(answers: &[&Answer], ok: impl Fn(&Answer) -> bool, what: &str, nothing: &str) -> Verdict {
    if answers.is_empty() {
        return unknown(nothing);
    }
    let bad: Vec<&Answer> = answers.iter().copied().filter(|a| !ok(a)).collect();
    if bad.is_empty() {
        met(format!("On every answer ({})", labels(answers)))
    } else {
        not_met(format!("{what}: {}", labels(&bad)))
    }
}

/// A CSP's directives by name, lower-case, from the answer: the first of a name wins.
fn csp_directives(a: &ProbeResponse) -> HashMap<String, Vec<String>> {
    let mut out = HashMap::new();
    for part in header(a, "content-security-policy").split([';', ',']) {
        let mut words = part.split_whitespace();
        if let Some(name) = words.next() {
            out.entry(name.to_lowercase())
                .or_insert_with(|| words.map(str::to_string).collect());
        }
    }
    out
}

struct Cookie {
    name: String,
    attrs: Vec<String>,
}

/// A Set-Cookie's name and its attributes, lower-case.
fn cookie(c: &str) -> Cookie {
    let mut parts = c.split(';');
    let pair = parts.next().unwrap_or("");
    Cookie {
        name: pair.split('=').next().unwrap_or("").trim().to_string(),
        attrs: parts.map(|x| x.trim().to_lowercase()).collect(),
    }
}

fn on_cookies(s: &Seen, ok: impl Fn(&Cookie) -> bool, what: &str) -> Verdict {
    let all: Vec<Cookie> = s
        .answers
        .iter()
        .flat_map(|a| a.resp.cookies.iter())
        .map(|c| cookie(c))
        .collect();
    if all.is_empty() {
        return unknown("No cookie was set on the answers read signed out");
    }
    let bad: Vec<&str> = all.iter().filter(|c| !ok(c)).map(|c| c.name.as_str()).collect();
    if bad.is_empty() {
        met(format!(
            "Every cookie ({})",
            all.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", ")
        ))
    } else {
        not_met(format!("{what}: {}", bad.join(", ")))
    }
}

/// Referrer policies that never send the path to another site.
const KEEPS_PATH: [&str; 6] = [
    "no-referrer",
    "same-origin",
    "strict-origin",
    "strict-origin-when-cross-origin",
    "origin",
    "origin-when-cross-origin",
];

/// What a stack trace, a file path or a runtime's internals look like in an error answer.
static LEAK: LazyLock<Regex> = LazyLock::new(|| {
    rx(
        r"\bat Object\.|\bat [\w$.<>]+ \(|Traceback \(most recent|[A-Za-z]:\\[\w\\]|/(home|usr|var|srv|opt|app)/\S+\.\w+|\.(ts|js|mjs|py|rb|php|java|go):\d+",
    )
});

/// `body.slice(0, n)` on characters.
fn head(body: &str, n: usize) -> &str {
    match body.char_indices().nth(n) {
        Some((i, _)) => &body[..i],
        None => body,
    }
}

fn http_redirect(s: &Seen) -> Verdict {
    let Some(r) = &s.plain else {
        return unknown("The http:// address didn't answer");
    };
    let to = header(r, "location");
    if [301, 302, 307, 308].contains(&r.status) && to.to_lowercase().starts_with("https://") {
        return met(format!("http:// answers {} to {to}", r.status));
    }
    let to_part = if to.is_empty() {
        String::new()
    } else {
        format!(" to {to}")
    };
    not_met(format!(
        "http:// answers {}{to_part}, not a redirect to https://",
        r.status
    ))
}

fn no_handshake(e: &str) -> Verdict {
    error(format!("No TLS handshake: {e}"))
}

fn tls_versions(s: &Seen) -> Verdict {
    let protocol = match &s.current {
        TlsAnswer::Failed { error, .. } => return no_handshake(error),
        TlsAnswer::Ok { protocol, .. } => protocol,
    };
    if protocol != "TLSv1.2" && protocol != "TLSv1.3" {
        return not_met(format!("The handshake settled on {protocol}, not TLS 1.2 or 1.3"));
    }
    let accepted: Vec<&str> = s
        .old
        .iter()
        .filter(|(_, a)| matches!(a, TlsAnswer::Ok { .. }))
        .map(|(v, _)| v.name())
        .collect();
    if !accepted.is_empty() {
        return not_met(format!("{} accepted", accepted.join(" and ")));
    }
    let untried: Vec<&str> = s
        .old
        .iter()
        .filter(|(_, a)| matches!(a, TlsAnswer::Failed { local: true, .. }))
        .map(|(v, _)| v.name())
        .collect();
    if !untried.is_empty() {
        return unknown(format!(
            "{protocol} accepted; {} couldn't be offered from this computer",
            untried.join(" and ")
        ));
    }
    met(format!("{protocol} accepted; TLS 1.0 and 1.1 refused"))
}

fn certificate(s: &Seen) -> Verdict {
    let (authorized, auth_error, valid_to) = match &s.current {
        TlsAnswer::Failed { error, .. } => return no_handshake(error),
        TlsAnswer::Ok {
            authorized,
            auth_error,
            valid_to,
            ..
        } => (*authorized, auth_error, valid_to),
    };
    if !authorized {
        let why = auth_error.as_deref().unwrap_or("unknown reason");
        return not_met(format!("The certificate isn't trusted: {why}"));
    }
    let Some(until) = valid_to else {
        return unknown("The certificate's expiry couldn't be read");
    };
    // `Math.floor`: a certificate that expired an hour ago is -1 days, not 0.
    let days = (*until - s.now).num_milliseconds().div_euclid(86_400_000);
    if days > CERT_MIN_DAYS {
        met(format!("Trusted, valid for {days} more days"))
    } else {
        not_met(format!("The certificate expires in {days} days"))
    }
}

fn hsts(s: &Seen) -> Verdict {
    static MAX_AGE: LazyLock<Regex> = LazyLock::new(|| rx(r"(?i)max-age=(\d+)"));
    on_every(
        &served(s),
        |a| {
            MAX_AGE
                .captures(header(&a.resp, "strict-transport-security"))
                .and_then(|c| c[1].parse::<u64>().ok())
                .unwrap_or(0)
                >= YEAR_S
        },
        "No Strict-Transport-Security with max-age of a year or more on",
        NO_PAGE,
    )
}

fn charset(s: &Seen) -> Verdict {
    static TEXTUAL: LazyLock<Regex> = LazyLock::new(|| rx(r"(?i)^(text/|[^;]+[/+]xml\b)"));
    static HAS_CHARSET: LazyLock<Regex> = LazyLock::new(|| rx(r"(?i);\s*charset="));
    let all = served(s);
    let bodies: Vec<&Answer> = all
        .into_iter()
        .filter(|a| a.resp.status != 204 && a.resp.status != 304 && !a.resp.body.is_empty())
        .collect();
    on_every(
        &bodies,
        |a| {
            let ct = header(&a.resp, "content-type");
            !ct.is_empty() && (!TEXTUAL.is_match(ct) || HAS_CHARSET.is_match(ct))
        },
        "No Content-Type, or a text one without a charset, on",
        NO_PAGE,
    )
}

fn api_not_renderable(s: &Seen) -> Verdict {
    if !s.target.retaskable {
        return unknown("Needs the project's own API routes");
    }
    let Some(api) = s.answers.iter().find(|a| a.kind == Kind::Api) else {
        return unknown(format!("{API_PATH} didn't answer"));
    };
    if header(&api.resp, "content-disposition")
        .trim_start()
        .to_lowercase()
        .starts_with("attachment")
    {
        return met("Content-Disposition: attachment");
    }
    if csp_directives(&api.resp).contains_key("sandbox") {
        return met("A CSP sandbox");
    }
    let n = s.navigated.as_ref();
    if let Some(n) = n
        && n.status != api.resp.status
        && [400, 403, 406].contains(&n.status)
    {
        return met(format!(
            "A request for it as a page (Sec-Fetch-Dest: document) is refused with {}",
            n.status
        ));
    }
    let got = n.map_or("nothing".to_string(), |n| n.status.to_string());
    not_met(format!(
        "{API_PATH} has no Content-Disposition: attachment or CSP sandbox, and answers a request for it as a page (Sec-Fetch-Dest: document) with {got}"
    ))
}

fn cors_origin(s: &Seen) -> Verdict {
    let rs: Vec<&ProbeResponse> = [&s.cors, &s.preflight].into_iter().flatten().collect();
    if rs.is_empty() {
        return unknown("Neither the request with an Origin nor its preflight was answered");
    }
    for r in rs {
        let allow = header(r, "access-control-allow-origin").trim();
        if allow == SCAN_ORIGIN || allow == "null" {
            return not_met(format!(
                "Access-Control-Allow-Origin answers {allow} to a request from {SCAN_ORIGIN}"
            ));
        }
        if allow == "*" && header(r, "access-control-allow-credentials").eq_ignore_ascii_case("true") {
            return not_met("Access-Control-Allow-Origin: * with credentials allowed");
        }
        if allow == "*" && s.target.retaskable {
            return not_met(format!(
                "Access-Control-Allow-Origin: * on {API_PATH}, which answers private data"
            ));
        }
    }
    met(format!("{SCAN_ORIGIN} isn't allowed"))
}

fn cookie_secure(s: &Seen) -> Verdict {
    on_cookies(
        s,
        |c| c.attrs.iter().any(|a| a == "secure") && (c.name.starts_with("__Host-") || c.name.starts_with("__Secure-")),
        "Not Secure, or without a __Host- or __Secure- name",
    )
}

fn csp_strict(s: &Seen) -> Verdict {
    static NONCE: LazyLock<Regex> = LazyLock::new(|| rx(r"^'(nonce|sha256|sha384|sha512)-"));
    let none = |v: Option<&Vec<String>>| v.is_some_and(|v| v.iter().any(|x| x == "'none'"));
    on_every(
        &pages(s),
        |a| {
            let d = csp_directives(&a.resp);
            let object = d.get("object-src").or_else(|| d.get("default-src"));
            let scripts = d.get("script-src").or_else(|| d.get("default-src"));
            none(object) && none(d.get("base-uri")) && scripts.is_some_and(|v| v.iter().any(|x| NONCE.is_match(x)))
        },
        "No CSP with object-src 'none', base-uri 'none' and scripts by nonce or hash on",
        NO_PAGE,
    )
}

fn nosniff(s: &Seen) -> Verdict {
    on_every(
        &served(s),
        |a| {
            header(&a.resp, "x-content-type-options")
                .trim()
                .eq_ignore_ascii_case("nosniff")
        },
        "No X-Content-Type-Options: nosniff on",
        NO_PAGE,
    )
}

fn referrer_policy(s: &Seen) -> Verdict {
    on_every(
        &served(s),
        |a| {
            // The last policy a browser knows wins.
            let last = header(&a.resp, "referrer-policy")
                .split(',')
                .map(|p| p.trim().to_lowercase())
                .rfind(|p| !p.is_empty());
            last.is_some_and(|l| KEEPS_PATH.contains(&l.as_str()))
        },
        "No Referrer-Policy that keeps the path from other sites on",
        NO_PAGE,
    )
}

fn frame_ancestors(s: &Seen) -> Verdict {
    on_every(
        &served(s),
        |a| csp_directives(&a.resp).contains_key("frame-ancestors"),
        "No frame-ancestors in a CSP on",
        NO_PAGE,
    )
}

fn cookie_samesite(s: &Seen) -> Verdict {
    on_cookies(
        s,
        |c| c.attrs.iter().any(|x| x == "samesite=lax" || x == "samesite=strict"),
        "No SameSite=Lax or Strict",
    )
}

fn cookie_host_prefix(s: &Seen) -> Verdict {
    on_cookies(s, |c| c.name.starts_with("__Host-"), "Not named __Host-")
}

fn cookie_httponly(s: &Seen) -> Verdict {
    on_cookies(s, |c| c.attrs.iter().any(|a| a == "httponly"), "Not HttpOnly")
}

fn error_leak(s: &Seen) -> Verdict {
    let errors: Vec<&Answer> = s
        .answers
        .iter()
        .filter(|a| a.kind == Kind::NotFound || a.kind == Kind::Api)
        .collect();
    on_every(
        &errors,
        |a| !LEAK.is_match(head(&a.resp.body, ERROR_TEXT_MAX)),
        "A stack trace or file path in",
        "Neither the 404 nor the API answered",
    )
}

fn signed_in_no_store(s: &Seen) -> Verdict {
    static NO_STORE: LazyLock<Regex> = LazyLock::new(|| rx(r"(?i)(^|,)\s*no-store\s*(,|$)"));
    if s.target.token.is_none() {
        return unknown("Not checked automatically: needs a staging test token (npm run scan-token)");
    }
    if s.target.signed_in_paths.is_empty() {
        return unknown("Not checked automatically: needs signed-in paths (scan.signedInPaths)");
    }
    let ok: Vec<&Answer> = s
        .signed_in
        .iter()
        .filter(|a| (200..300).contains(&a.resp.status))
        .collect();
    if ok.is_empty() {
        let seen: Vec<String> = s
            .signed_in
            .iter()
            .map(|a| format!("{} {}", a.label, a.resp.status))
            .collect();
        let seen = if seen.is_empty() {
            "no answer".to_string()
        } else {
            seen.join(", ")
        };
        return unknown(format!("No signed-in path answered with the test token ({seen})"));
    }
    on_every(
        &ok,
        |a| NO_STORE.is_match(header(&a.resp, "cache-control")),
        "No Cache-Control: no-store on the signed-in answer of",
        NO_PAGE,
    )
}

fn coop(s: &Seen) -> Verdict {
    static SAME: LazyLock<Regex> = LazyLock::new(|| rx(r"(?i)^\s*same-origin(-allow-popups)?\s*$"));
    let html: Vec<&Answer> = served(s).into_iter().filter(|a| is_html(&a.resp)).collect();
    on_every(
        &html,
        |a| SAME.is_match(header(&a.resp, "cross-origin-opener-policy")),
        "No Cross-Origin-Opener-Policy: same-origin on",
        "No HTML page answered",
    )
}

fn sri(s: &Seen) -> Verdict {
    let html: Vec<&Answer> = pages(s).into_iter().filter(|a| is_html(&a.resp)).collect();
    if html.is_empty() {
        return unknown("No HTML page answered");
    }
    let host = super::lighthouse_summary::host_of(&s.target.url);
    let mut bare: Vec<String> = Vec::new();
    for a in html {
        for src in foreign_without_integrity(&a.resp.body, &host) {
            if !bare.contains(&src) {
                bare.push(src);
            }
        }
    }
    if bare.is_empty() {
        met("Every script and stylesheet from another site carries integrity, or there are none")
    } else {
        not_met(format!(
            "Without integrity: {}",
            bare.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        ))
    }
}

fn server_version(s: &Seen) -> Verdict {
    on_every(
        &served(s),
        |a| {
            !header(&a.resp, "server").chars().any(|c| c.is_ascii_digit())
                && !header(&a.resp, "x-powered-by").chars().any(|c| c.is_ascii_digit())
        },
        "A version in Server or X-Powered-By on",
        NO_PAGE,
    )
}

fn trusted_types(s: &Seen) -> Verdict {
    on_every(
        &pages(s),
        |a| {
            csp_directives(&a.resp)
                .get("require-trusted-types-for")
                .is_some_and(|v| v.iter().any(|x| x == "'script'"))
        },
        "No require-trusted-types-for 'script' on",
        NO_PAGE,
    )
}

fn permissions_policy(s: &Seen) -> Verdict {
    on_every(
        &pages(s),
        |a| !header(&a.resp, "permissions-policy").trim().is_empty(),
        "No Permissions-Policy on",
        NO_PAGE,
    )
}

fn security_txt(s: &Seen) -> Verdict {
    static CONTACT: LazyLock<Regex> = LazyLock::new(|| rx(r"(?im)^contact:\s*\S"));
    static EXPIRES: LazyLock<Regex> = LazyLock::new(|| rx(r"(?im)^expires:\s*(.+)$"));
    let Some(r) = &s.security_txt else {
        return unknown("/.well-known/security.txt didn't answer");
    };
    let ct = header(r, "content-type");
    if r.status != 200 || !ct.to_lowercase().starts_with("text/plain") {
        return not_met(
            format!("/.well-known/security.txt answers {} {ct}", r.status)
                .trim()
                .to_string(),
        );
    }
    if !CONTACT.is_match(&r.body) {
        return not_met("security.txt has no Contact:");
    }
    let expires = EXPIRES.captures(&r.body).and_then(|c| parse_date(c[1].trim()));
    match expires {
        Some(e) if e > s.now => met("security.txt with a Contact: and an Expires: in the future"),
        _ => not_met("security.txt has no Expires: in the future"),
    }
}

/// `Date.parse` for what security.txt's `Expires:` has (RFC 3339), and a bare day.
fn parse_date(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .ok()
        .or_else(|| DateTime::parse_from_rfc2822(s).map(|d| d.with_timezone(&Utc)).ok())
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|d| d.and_utc())
        })
}

/// The `<script src>` and stylesheet `<link href>` of `html` on another host that carry no `integrity`. Pure.
pub fn foreign_without_integrity(html: &str, host: &str) -> Vec<String> {
    static TAG: LazyLock<Regex> = LazyLock::new(|| rx(r"(?i)<(script|link)\b[^>]*>"));
    static STYLESHEET: LazyLock<Regex> = LazyLock::new(|| rx(r#"(?i)\brel\s*=\s*["']?[^"'>]*\bstylesheet\b"#));
    static SRC: LazyLock<Regex> = LazyLock::new(|| rx(r#"(?i)\b(?:src|href)\s*=\s*["']?([^"'\s>]+)"#));
    static ABSOLUTE: LazyLock<Regex> = LazyLock::new(|| rx(r"(?i)^(https?:)?//"));
    static INTEGRITY: LazyLock<Regex> = LazyLock::new(|| rx(r"(?i)\bintegrity\s*="));
    let base = reqwest::Url::parse(&format!("https://{host}")).ok();
    let mut out = Vec::new();
    for m in TAG.captures_iter(html) {
        let tag = &m[0];
        if m[1].eq_ignore_ascii_case("link") && !STYLESHEET.is_match(tag) {
            continue;
        }
        let Some(src) = SRC.captures(tag).map(|c| c[1].to_string()) else {
            continue;
        };
        if !ABSOLUTE.is_match(&src) {
            continue;
        }
        let Some(other) = base.as_ref().and_then(|b| b.join(&src).ok()) else {
            continue;
        };
        if super::lighthouse_summary::host_of(other.as_str()) != host && !INTEGRITY.is_match(tag) {
            out.push(src);
        }
    }
    out
}

#[cfg(test)]
mod tests;
