use std::sync::Mutex;

use chrono::TimeZone;
use serde_json::json;

use super::*;
use crate::jobs::probes_checks::Status;

type Sent = (String, Method, Vec<(String, String)>);

/// A fake site: answers by (method, url), records every request and every wait.
#[derive(Default)]
struct Site {
    answers: Mutex<Vec<(String, Method, ProbeResponse)>>,
    requests: Mutex<Vec<Sent>>,
    waits: Mutex<Vec<Duration>>,
    tls: Mutex<Vec<(String, u16, Option<TlsVersion>)>>,
}

impl Site {
    fn answer(&self, url: &str, method: Method, r: ProbeResponse) {
        self.answers.lock().unwrap().push((url.to_string(), method, r));
    }
}

impl ProbeDeps for Site {
    async fn fetch(&self, url: &str, method: Method, headers: &[(&str, String)]) -> Result<ProbeResponse, String> {
        self.requests.lock().unwrap().push((
            url.to_string(),
            method,
            headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect(),
        ));
        self.answers
            .lock()
            .unwrap()
            .iter()
            .find(|(u, m, _)| u == url && *m == method)
            .map(|(_, _, r)| r.clone())
            .ok_or_else(|| "no answer".to_string())
    }

    async fn tls(&self, host: &str, port: u16, version: Option<TlsVersion>) -> TlsAnswer {
        self.tls.lock().unwrap().push((host.to_string(), port, version));
        match version {
            None => TlsAnswer::Ok {
                protocol: "TLSv1.3".into(),
                authorized: true,
                auth_error: None,
                valid_to: Some(self.now() + chrono::Duration::days(90)),
            },
            Some(_) => TlsAnswer::Failed {
                local: false,
                error: "refused".into(),
            },
        }
    }

    async fn wait(&self, d: Duration) {
        self.waits.lock().unwrap().push(d);
    }

    fn now(&self) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap()
    }
}

fn ok(headers: &[(&str, &str)], body: &str) -> ProbeResponse {
    ProbeResponse {
        status: 200,
        headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        cookies: vec![],
        body: body.to_string(),
    }
}

fn settings() -> ScanSettings {
    ScanSettings {
        probes: true,
        staging_url: Some("https://stage.example:8443/".into()),
        pages: vec!["/".into(), "docs".into()],
        signed_in_paths: vec!["api/me".into()],
        ..Default::default()
    }
}

fn job(args: serde_json::Value) -> JobSpec {
    JobSpec::from_json(&json!({ "kind": "probes", "day": "2026-11-17", "args": args })).unwrap()
}

fn token(_: &str) -> Option<String> {
    Some("s3cret".to_string())
}

fn no_token(_: &str) -> Option<String> {
    None
}

#[test]
fn the_target_is_off_a_note_or_the_origin_and_pages() {
    let mut s = settings();
    s.probes = false;
    assert_eq!(probes_target(&s), None);
    s.probes = true;
    s.staging_url = None;
    assert_eq!(
        probes_target(&s),
        Some(Err(
            "The probes need a staging url (deploy.staging.url) to run on.".to_string()
        ))
    );
    let t = probes_target(&settings()).unwrap().unwrap();
    assert_eq!(t.url, "https://stage.example:8443");
    assert_eq!(
        (t.pages, t.signed_in_paths),
        (vec!["/".to_string(), "/docs".to_string()], vec!["/api/me".to_string()])
    );
    assert!(!t.retaskable && t.token.is_none());
    let mut own = settings();
    own.staging_url = Some("https://staging.retaskable.com".into());
    own.pages.clear();
    let t = probes_target(&own).unwrap().unwrap();
    assert!(t.retaskable);
    assert_eq!(t.pages, ["/"]);
}

#[test]
fn the_scan_token_fact_needs_the_probes_on_and_a_token() {
    assert!(has_scan_token(&settings(), &token));
    assert!(!has_scan_token(&settings(), &no_token));
    let mut off = settings();
    off.probes = false;
    assert!(!has_scan_token(&off, &token));
    off.probes = true;
    off.staging_url = None;
    assert!(!has_scan_token(&off, &token));
}

#[tokio::test]
async fn it_refuses_when_off_unrunnable_or_pointed_elsewhere() {
    let site = Site::default();
    let mut off = settings();
    off.probes = false;
    let j = job(json!({}));
    let input = ProbesInput {
        job: &j,
        settings: &off,
        token_for: &token,
        known: BTreeMap::new(),
    };
    assert_eq!(
        probes_job(&input, &site).await,
        Outcome::refused("The security probes aren't on for this project.")
    );
    let on = settings();
    let elsewhere = job(json!({ "url": "https://evil.example" }));
    let input = ProbesInput {
        job: &elsewhere,
        settings: &on,
        token_for: &token,
        known: BTreeMap::new(),
    };
    assert_eq!(
        probes_job(&input, &site).await,
        Outcome::refused("The probes won't run: the task's url isn't this project's staging url.")
    );
    assert!(site.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn the_requests_are_read_only_spaced_and_stay_on_the_staging_host() {
    let site = Site::default();
    let (s, j) = (settings(), job(json!({ "url": "https://stage.example:8443/x" })));
    let input = ProbesInput {
        job: &j,
        settings: &s,
        token_for: &token,
        known: BTreeMap::new(),
    };
    let out = probes_job(&input, &site).await;
    assert!(matches!(out, Outcome::Done { .. }));
    let reqs = site.requests.lock().unwrap();
    assert!(
        reqs.iter().all(
            |(u, _, _)| u.starts_with("https://stage.example:8443/") || u.starts_with("http://stage.example:8443/")
        )
    );
    assert!(reqs.iter().all(|(_, m, _)| matches!(m, Method::Get | Method::Options)));
    let urls: Vec<&str> = reqs.iter().map(|(u, _, _)| u.as_str()).collect();
    assert_eq!(
        urls,
        [
            "https://stage.example:8443/",
            "https://stage.example:8443/docs",
            "https://stage.example:8443/retaskable-scan-not-found",
            "https://stage.example:8443/",
            "https://stage.example:8443/",
            "http://stage.example:8443/",
            "https://stage.example:8443/.well-known/security.txt",
            "https://stage.example:8443/api/me",
        ]
    );
    assert_eq!(reqs[3].2, [("Origin".to_string(), "https://scan.invalid".to_string())]);
    assert_eq!(reqs[4].1, Method::Options);
    // The token goes only on the signed-in path.
    let with_token: Vec<&str> = reqs
        .iter()
        .filter(|(_, _, h)| h.iter().any(|(k, _)| k == "Authorization"))
        .map(|(u, _, _)| u.as_str())
        .collect();
    assert_eq!(with_token, ["https://stage.example:8443/api/me"]);
    // One wait between each pair of requests, never a sleep of ours.
    assert_eq!(site.waits.lock().unwrap().len(), reqs.len() - 1);
    assert!(site.waits.lock().unwrap().iter().all(|d| *d == SPACING));
    let tls = site.tls.lock().unwrap();
    assert_eq!(tls.len(), 3);
    assert_eq!(tls[0], ("stage.example".to_string(), 8443, None));
}

#[tokio::test]
async fn the_retaskable_site_is_probed_at_its_own_api_too() {
    let site = Site::default();
    let mut s = settings();
    s.staging_url = Some("https://staging.retaskable.com".into());
    s.signed_in_paths.clear();
    let j = job(json!({}));
    let input = ProbesInput {
        job: &j,
        settings: &s,
        token_for: &no_token,
        known: BTreeMap::new(),
    };
    probes_job(&input, &site).await;
    let reqs = site.requests.lock().unwrap();
    let api: Vec<&Method> = reqs
        .iter()
        .filter(|(u, _, _)| u.ends_with("/api/me"))
        .map(|(_, m, _)| m)
        .collect();
    assert_eq!(api.len(), 3, "the CORS read, its preflight and the as-a-page read");
    assert!(
        reqs.iter()
            .filter(|(_, _, h)| h.iter().any(|(k, _)| k == "Sec-Fetch-Dest"))
            .count()
            == 1
    );
    assert!(reqs.iter().all(|(_, _, h)| h.iter().all(|(k, _)| k != "Authorization")));
    assert_eq!(site.tls.lock().unwrap()[0].1, 443);
}

#[tokio::test]
async fn the_result_has_statuses_what_was_not_met_and_never_the_token() {
    let site = Site::default();
    site.answer(
        "https://stage.example:8443/",
        Method::Get,
        ok(
            &[("content-type", "text/html; charset=utf-8"), ("server", "nginx/1.2")],
            "<p>hi",
        ),
    );
    site.answer(
        "http://stage.example:8443/",
        Method::Get,
        ProbeResponse {
            status: 301,
            headers: [("location".to_string(), "https://stage.example:8443/".to_string())].into(),
            ..Default::default()
        },
    );
    let (s, j) = (settings(), job(json!({})));
    let known = BTreeMap::from([("k".to_string(), "d".to_string())]);
    let input = ProbesInput {
        job: &j,
        settings: &s,
        token_for: &token,
        known: known.clone(),
    };
    let Outcome::Done { result, known: k } = probes_job(&input, &site).await else {
        panic!()
    };
    assert_eq!(k, known);
    assert_eq!(result.day, "2026-11-17");
    assert_eq!(result.results.len(), 23);
    let status = |p: &str| result.results.iter().find(|r| r.probe == p).unwrap().status;
    assert_eq!(status("http-redirect"), Status::Met);
    assert_eq!(status("server-version"), Status::NotMet);
    assert_eq!(status("tls-versions"), Status::Met);
    assert_eq!(status("sri"), Status::Met);
    assert_eq!(status("signed-in-no-store"), Status::Unknown);
    let seen: Vec<&str> = result.seen.iter().map(|s| s.probe).collect();
    assert!(seen.contains(&"server-version") && seen.contains(&"hsts"));
    let text = serde_json::to_string(&result).unwrap();
    assert!(!text.contains("s3cret"), "no secret in the result");
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["results"][0], json!({ "probe": "http-redirect", "status": "met" }));
    assert_eq!(v["results"][1]["status"], "met");
    assert!(v.get("note").is_none());
    assert!(
        v["seen"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["detail"].as_str().unwrap().chars().count() <= 300)
    );
}

#[tokio::test]
async fn failed_probes_are_a_note() {
    struct NoTls(Site);
    impl ProbeDeps for NoTls {
        async fn fetch(&self, u: &str, m: Method, h: &[(&str, String)]) -> Result<ProbeResponse, String> {
            self.0.fetch(u, m, h).await
        }
        async fn tls(&self, _: &str, _: u16, _: Option<TlsVersion>) -> TlsAnswer {
            TlsAnswer::Failed {
                local: false,
                error: "connection refused".into(),
            }
        }
        async fn wait(&self, d: Duration) {
            self.0.wait(d).await
        }
        fn now(&self) -> DateTime<Utc> {
            self.0.now()
        }
    }
    let (s, j) = (settings(), job(json!({})));
    let input = ProbesInput {
        job: &j,
        settings: &s,
        token_for: &no_token,
        known: BTreeMap::new(),
    };
    let Outcome::Done { result, .. } = probes_job(&input, &NoTls(Site::default())).await else {
        panic!()
    };
    assert_eq!(
        result.note.as_deref(),
        Some("2 probes failed: tls-versions: No TLS handshake: connection refused")
    );
    let statuses: Vec<Status> = result
        .results
        .iter()
        .filter(|r| r.probe == "certificate")
        .map(|r| r.status)
        .collect();
    assert_eq!(statuses, [Status::Error]);
}

#[tokio::test]
async fn the_real_deps_read_headers_cookies_and_a_get_body_without_following_redirects() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 2048];
            let n = sock.read(&mut buf).await.unwrap();
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let reply = if head.starts_with("GET /go") {
                "HTTP/1.1 302 Found\r\nLocation: /there\r\nSet-Cookie: a=1; Secure\r\nSet-Cookie: b=2\r\nX-Multi: one\r\nX-Multi: two\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            } else {
                "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n"
            };
            sock.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    let deps = RealProbeDeps::new();
    let r = deps
        .fetch(
            &format!("http://{addr}/go"),
            Method::Get,
            &[("Origin", "https://scan.invalid".into())],
        )
        .await
        .unwrap();
    assert_eq!(r.status, 302);
    assert_eq!(r.headers["location"], "/there");
    assert_eq!(r.headers["x-multi"], "one, two");
    assert_eq!(r.cookies, ["a=1; Secure", "b=2"]);
    let r = deps
        .fetch(&format!("http://{addr}/x"), Method::Options, &[])
        .await
        .unwrap();
    assert_eq!((r.status, r.body.as_str()), (204, ""));
    server.await.unwrap();
    assert!(deps.fetch("http://127.0.0.1:1/", Method::Get, &[]).await.is_err());
    // The handshake isn't built: it says so, rather than guessing.
    assert!(matches!(
        deps.tls("h", 443, None).await,
        TlsAnswer::Failed { local: true, .. }
    ));
}
