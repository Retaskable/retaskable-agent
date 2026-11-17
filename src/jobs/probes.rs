//! A `probes` task (`probes()` in `jobTasks.ts`, `securityProbes.ts` and `scanChore.ts`; compliance-scan.md section
//! 3.2): the daily probes of the staging site. Plain HTTP and TLS, read-only (GET, HEAD and OPTIONS only), about a
//! dozen requests spaced apart, each check giving evidence for the ASVS 5.0 requirement it names (or for an Extra
//! hardening check) in [`super::probes_checks`]. No model. The test token is read at the run, so it is never in the
//! target or the result.
use std::collections::BTreeMap;
use std::future::Future;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::Serialize;

use super::probes_checks::{API_PATH, Answer, CHECKS, Kind, SCAN_ORIGIN, Seen, Status};
use super::scan::{PROBES_WORDS, ScanSettings, as_path, is_retaskable};
use super::{JobSpec, Outcome, one_line, same_origin, wrong_url};

/// The made-up path whose answer is the site's 404.
pub const NOT_FOUND_PATH: &str = "/retaskable-scan-not-found";
const MAX_PAGES: usize = 5;
/// Between requests, so a rate limit never answers 429 (cloud.md section 7).
pub const SPACING: Duration = Duration::from_millis(250);
/// `SCAN_PROBES_RAW_NOTE_MAX`.
pub const NOTE_MAX: usize = 2000;

/// What the probes look at: the staging site, never production.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeTarget {
    /// The staging url's origin, without a trailing slash.
    pub url: String,
    /// Pages to read, paths starting with "/".
    pub pages: Vec<String>,
    /// This site's own routes are there (`/api/me`), for the checks that need them.
    pub retaskable: bool,
    /// Signed-in API paths (`scan.signedInPaths`), read with `token` for 14.3.2.
    pub signed_in_paths: Vec<String>,
    /// The staging test token, sent as `Authorization: Bearer` on those GETs only. Never in a detail.
    pub token: Option<String>,
}

/// `probesTarget`: what the probes look at, `Err(note)` when they're on but can't run, or `None` when they're off.
pub fn probes_target(s: &ScanSettings) -> Option<Result<ProbeTarget, String>> {
    if !s.probes {
        return None;
    }
    let (origin, url) = match s.staging_origin(&PROBES_WORDS) {
        Ok(v) => v,
        Err(note) => return Some(Err(note)),
    };
    let pages = s.listed_pages();
    let host = url.host_str().unwrap_or("");
    Some(Ok(ProbeTarget {
        url: origin,
        pages: if pages.is_empty() { vec!["/".to_string()] } else { pages },
        retaskable: is_retaskable(host),
        signed_in_paths: s
            .signed_in_paths
            .iter()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .map(as_path)
            .collect(),
        token: None,
    }))
}

/// Whether `can` says `scanToken`: the probes are on, can run, and the staging origin has a saved test token.
pub fn has_scan_token(s: &ScanSettings, token_for: &dyn Fn(&str) -> Option<String>) -> bool {
    matches!(probes_target(s), Some(Ok(t)) if token_for(&t.url).is_some())
}

/// The methods the probes send, and no others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Options,
}

impl Method {
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Options => "OPTIONS",
        }
    }
}

/// One answer as the probes read it: header names lower-case, each `Set-Cookie` apart.
#[derive(Debug, Clone, Default)]
pub struct ProbeResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub cookies: Vec<String>,
    pub body: String,
}

/// The old TLS versions the probes try to offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsVersion {
    V1_0,
    V1_1,
}

impl TlsVersion {
    pub fn name(self) -> &'static str {
        match self {
            TlsVersion::V1_0 => "TLSv1",
            TlsVersion::V1_1 => "TLSv1.1",
        }
    }
}

/// A TLS handshake's answer (`TlsAnswer`).
#[derive(Debug, Clone, PartialEq)]
pub enum TlsAnswer {
    Ok {
        protocol: String,
        authorized: bool,
        auth_error: Option<String>,
        valid_to: Option<DateTime<Utc>>,
    },
    /// `local`: this computer's TLS library couldn't offer that version, so the server wasn't asked.
    Failed { local: bool, error: String },
}

/// What the probes do outside themselves: real ones are [`RealProbeDeps`], fakes in the tests.
pub trait ProbeDeps {
    /// Never follows a redirect. `Err` when nothing answered.
    fn fetch(
        &self,
        url: &str,
        method: Method,
        headers: &[(&str, String)],
    ) -> impl Future<Output = Result<ProbeResponse, String>> + Send;
    /// A TLS handshake with `host`, allowing only `version` when given.
    fn tls(&self, host: &str, port: u16, version: Option<TlsVersion>) -> impl Future<Output = TlsAnswer> + Send;
    fn wait(&self, d: Duration) -> impl Future<Output = ()> + Send;
    fn now(&self) -> DateTime<Utc>;
}

/// One probe's result: its status and what was seen, for the write-up's prompt; never the public page.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeResult {
    pub probe: &'static str,
    pub status: Status,
    pub detail: String,
}

/// Runs every probe against `t`. Pages that don't answer are left out, and a check with nothing to look at is
/// `unknown`. Every request goes to the staging url's own host. Answers the results and how many requests went.
pub async fn run_probes<D: ProbeDeps>(t: &ProbeTarget, deps: &D) -> (Vec<ProbeResult>, usize) {
    let origin = reqwest::Url::parse(&t.url).expect("a target's url is an origin");
    let host = super::lighthouse_summary::host_of(&t.url);
    let scheme = origin.scheme().to_string();
    let mut requests = 0usize;
    let mut get = async |path: &str, headers: &[(&str, String)], method: Method, scheme: &str| {
        if requests > 0 {
            deps.wait(SPACING).await;
        }
        requests += 1;
        deps.fetch(&format!("{scheme}://{host}{path}"), method, headers)
            .await
            .ok()
    };
    let mut answers: Vec<Answer> = Vec::new();
    let add = |answers: &mut Vec<Answer>, kind: Kind, label: &str, r: &Option<ProbeResponse>| {
        if let Some(resp) = r {
            answers.push(Answer {
                resp: resp.clone(),
                kind,
                label: label.to_string(),
            });
        }
    };
    for page in t.pages.iter().take(MAX_PAGES) {
        let r = get(page, &[], Method::Get, &scheme).await;
        add(&mut answers, Kind::Page, page, &r);
    }
    let r = get(NOT_FOUND_PATH, &[], Method::Get, &scheme).await;
    add(&mut answers, Kind::NotFound, "the 404", &r);
    let first = t.pages.first().map_or("/", String::as_str);
    let cors_path = if t.retaskable { API_PATH } else { first };
    let cors = get(cors_path, &[("Origin", SCAN_ORIGIN.to_string())], Method::Get, &scheme).await;
    if t.retaskable {
        add(&mut answers, Kind::Api, "the API answer", &cors);
    }
    let preflight = get(
        cors_path,
        &[
            ("Origin", SCAN_ORIGIN.to_string()),
            ("Access-Control-Request-Method", "GET".to_string()),
        ],
        Method::Options,
        &scheme,
    )
    .await;
    let navigated = if t.retaskable {
        get(
            API_PATH,
            &[
                ("Sec-Fetch-Dest", "document".to_string()),
                ("Sec-Fetch-Mode", "navigate".to_string()),
                ("Sec-Fetch-Site", "none".to_string()),
            ],
            Method::Get,
            &scheme,
        )
        .await
    } else {
        None
    };
    let plain = get(first, &[], Method::Get, "http").await;
    let security_txt = get("/.well-known/security.txt", &[], Method::Get, &scheme).await;
    add(&mut answers, Kind::SecurityTxt, "security.txt", &security_txt);
    let mut signed_in = Vec::new();
    if let Some(token) = &t.token {
        for path in t.signed_in_paths.iter().take(MAX_PAGES) {
            let auth = [("Authorization", format!("Bearer {token}"))];
            if let Some(resp) = get(path, &auth, Method::Get, &scheme).await {
                signed_in.push(Answer {
                    resp,
                    kind: Kind::Api,
                    label: path.clone(),
                });
            }
        }
    }
    let port = origin.port().unwrap_or(443);
    let hostname = origin.host_str().unwrap_or("");
    let current = deps.tls(hostname, port, None).await;
    let mut old = Vec::new();
    for v in [TlsVersion::V1_0, TlsVersion::V1_1] {
        old.push((v, deps.tls(hostname, port, Some(v)).await));
    }
    let seen = Seen {
        target: t,
        answers,
        signed_in,
        cors,
        preflight,
        navigated,
        plain,
        security_txt,
        current,
        old,
        now: deps.now(),
    };
    let results = CHECKS
        .iter()
        .map(|(probe, check)| {
            // A check that panics is that probe's `error`, as one that throws is in TS.
            let v = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&seen)));
            match v {
                Ok(v) => ProbeResult {
                    probe,
                    status: v.status,
                    detail: v.detail,
                },
                Err(_) => ProbeResult {
                    probe,
                    status: Status::Error,
                    detail: "the check failed".to_string(),
                },
            }
        })
        .collect();
    (results, requests)
}

/// One probe's status in the raw result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusOnly {
    pub probe: &'static str,
    pub status: Status,
}

/// What a probe that found a requirement not met saw: the store's write-up uses it and keeps none of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Seen1 {
    pub probe: &'static str,
    pub detail: String,
}

/// The day's raw result (`ScanProbesRaw`): `scan_probes_raw` in the store.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProbesResult {
    pub day: String,
    pub results: Vec<StatusOnly>,
    pub ms: u64,
    pub seen: Vec<Seen1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// What the task and this computer's settings give the run.
pub struct ProbesInput<'a> {
    /// `day` and `args.url`.
    pub job: &'a JobSpec,
    pub settings: &'a ScanSettings,
    /// The staging origin's saved test token (`stagingToken`): read at the run, `None` when it isn't set or the
    /// credential store can't be read.
    pub token_for: &'a (dyn Fn(&str) -> Option<String> + Sync),
    /// An old agent's own record of the findings it wrote up.
    pub known: BTreeMap<String, String>,
}

/// The `probes` job.
pub async fn probes_job<D: ProbeDeps>(input: &ProbesInput<'_>, deps: &D) -> Outcome<ProbesResult> {
    let mut t = match probes_target(input.settings) {
        None => return Outcome::refused("The security probes aren't on for this project."),
        Some(Err(note)) => return Outcome::refused(note),
        Some(Ok(t)) => t,
    };
    if let Some(url) = input.job.arg_str("url")
        && !same_origin(url, &t.url)
    {
        return wrong_url("The probes won't run");
    }
    let started = Instant::now();
    t.token = (input.token_for)(&t.url);
    let (results, _requests) = run_probes(&t, deps).await;
    let failed: Vec<&ProbeResult> = results.iter().filter(|r| r.status == Status::Error).collect();
    let note = failed.first().map(|f| {
        let n = failed.len();
        one_line(
            &format!(
                "{n} {} failed: {}: {}",
                if n == 1 { "probe" } else { "probes" },
                f.probe,
                f.detail
            ),
            NOTE_MAX,
        )
    });
    let seen = results
        .iter()
        .filter(|r| r.status == Status::NotMet)
        .map(|r| Seen1 {
            probe: r.probe,
            detail: r.detail.chars().take(300).collect(),
        })
        .collect();
    Outcome::Done {
        result: ProbesResult {
            day: input.job.day.clone(),
            results: results
                .iter()
                .map(|r| StatusOnly {
                    probe: r.probe,
                    status: r.status,
                })
                .collect(),
            ms: started.elapsed().as_millis() as u64,
            seen,
            note,
        },
        known: input.known.clone(),
    }
}

/// The real deps: reqwest (no redirects followed, 15 seconds each), tokio's timer and the clock. The TLS handshake
/// is not built yet: see [`RealProbeDeps::tls`].
pub struct RealProbeDeps {
    client: reqwest::Client,
}

impl RealProbeDeps {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(15))
            .build()
            .expect("an HTTP client with no redirects");
        RealProbeDeps { client }
    }
}

impl Default for RealProbeDeps {
    fn default() -> Self {
        Self::new()
    }
}

impl ProbeDeps for RealProbeDeps {
    async fn fetch(&self, url: &str, method: Method, headers: &[(&str, String)]) -> Result<ProbeResponse, String> {
        let m = reqwest::Method::from_bytes(method.as_str().as_bytes()).map_err(|e| e.to_string())?;
        let mut req = self.client.request(m, url);
        for (k, v) in headers {
            req = req.header(*k, v);
        }
        let r = req.send().await.map_err(|e| e.to_string())?;
        let status = r.status().as_u16();
        let mut map: BTreeMap<String, String> = BTreeMap::new();
        let mut cookies = Vec::new();
        for (k, v) in r.headers() {
            let v = String::from_utf8_lossy(v.as_bytes()).into_owned();
            if k.as_str() == "set-cookie" {
                cookies.push(v.clone());
            }
            map.entry(k.as_str().to_lowercase())
                .and_modify(|old| {
                    old.push_str(", ");
                    old.push_str(&v);
                })
                .or_insert(v);
        }
        let body = if method == Method::Get {
            r.text().await.map_err(|e| e.to_string())?
        } else {
            String::new()
        };
        Ok(ProbeResponse {
            status,
            headers: map,
            cookies,
            body,
        })
    }

    /// Not built: reading a certificate's expiry and offering TLS 1.0/1.1 needs a TLS client this crate doesn't
    /// depend on yet (rustls offers neither old versions nor the certificate's dates). Answers a local failure, so
    /// `tls-versions` and `certificate` say `error` rather than guess.
    async fn tls(&self, _host: &str, _port: u16, _version: Option<TlsVersion>) -> TlsAnswer {
        TlsAnswer::Failed {
            local: true,
            error: "this build of the agent can't make a TLS handshake yet".to_string(),
        }
    }

    async fn wait(&self, d: Duration) {
        tokio::time::sleep(d).await;
    }

    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

#[cfg(test)]
mod tests;
