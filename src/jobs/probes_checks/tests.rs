use chrono::TimeZone;

use super::*;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap()
}

fn resp(status: u16, headers: &[(&str, &str)], body: &str) -> ProbeResponse {
    ProbeResponse {
        status,
        headers: headers.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        cookies: Vec::new(),
        body: body.to_string(),
    }
}

fn page(label: &str, r: ProbeResponse) -> Answer {
    Answer {
        resp: r,
        kind: Kind::Page,
        label: label.to_string(),
    }
}

fn target(retaskable: bool) -> ProbeTarget {
    ProbeTarget {
        url: "https://s.example".to_string(),
        pages: vec!["/".to_string()],
        retaskable,
        signed_in_paths: Vec::new(),
        token: None,
    }
}

fn tls_ok(days: i64) -> TlsAnswer {
    TlsAnswer::Ok {
        protocol: "TLSv1.3".to_string(),
        authorized: true,
        auth_error: None,
        valid_to: Some(now() + chrono::Duration::days(days)),
    }
}

fn refused() -> TlsAnswer {
    TlsAnswer::Failed {
        local: false,
        error: "handshake failure".to_string(),
    }
}

fn seen<'a>(t: &'a ProbeTarget, answers: Vec<Answer>) -> Seen<'a> {
    Seen {
        target: t,
        answers,
        signed_in: Vec::new(),
        cors: None,
        preflight: None,
        navigated: None,
        plain: None,
        security_txt: None,
        current: tls_ok(60),
        old: vec![(TlsVersion::V1_0, refused()), (TlsVersion::V1_1, refused())],
        now: now(),
    }
}

fn run(probe: &str, s: &Seen) -> Verdict {
    let (_, check) = CHECKS.iter().find(|(p, _)| *p == probe).unwrap();
    check(s)
}

fn status(probe: &str, s: &Seen) -> Status {
    run(probe, s).status
}

#[test]
fn the_ids_are_the_asvs_probe_list_in_order() {
    let ids: Vec<&str> = CHECKS.iter().map(|(p, _)| *p).collect();
    assert_eq!(ids.len(), 23);
    assert_eq!(&ids[..4], ["http-redirect", "tls-versions", "certificate", "hsts"]);
    assert_eq!(ids[22], "security-txt");
}

#[test]
fn a_site_that_answers_nothing_is_unknown_everywhere_a_page_is_needed() {
    let t = target(false);
    let s = seen(&t, vec![]);
    for p in [
        "hsts",
        "charset",
        "csp-strict",
        "nosniff",
        "referrer-policy",
        "frame-ancestors",
        "server-version",
        "trusted-types",
        "permissions-policy",
    ] {
        assert_eq!(run(p, &s), unknown("No page of the staging site answered"), "{p}");
    }
    assert_eq!(run("coop", &s).detail, "No HTML page answered");
    assert_eq!(run("error-leak", &s).detail, "Neither the 404 nor the API answered");
    assert_eq!(status("sri", &s), Status::Unknown);
    assert_eq!(
        run("cookie-secure", &s).detail,
        "No cookie was set on the answers read signed out"
    );
    assert_eq!(status("http-redirect", &s), Status::Unknown);
    assert_eq!(status("security-txt", &s), Status::Unknown);
    assert_eq!(status("cors-origin", &s), Status::Unknown);
}

#[test]
fn http_redirect_wants_a_redirect_to_https() {
    let t = target(false);
    let mut s = seen(&t, vec![]);
    s.plain = Some(resp(301, &[("location", "https://s.example/")], ""));
    assert_eq!(
        run("http-redirect", &s),
        met("http:// answers 301 to https://s.example/")
    );
    s.plain = Some(resp(200, &[], ""));
    assert_eq!(
        run("http-redirect", &s),
        not_met("http:// answers 200, not a redirect to https://")
    );
    s.plain = Some(resp(302, &[("location", "http://s.example/")], ""));
    assert_eq!(
        run("http-redirect", &s).detail,
        "http:// answers 302 to http://s.example/, not a redirect to https://"
    );
}

#[test]
fn tls_checks_read_the_handshakes() {
    let t = target(false);
    let mut s = seen(&t, vec![]);
    assert_eq!(
        run("tls-versions", &s),
        met("TLSv1.3 accepted; TLS 1.0 and 1.1 refused")
    );
    assert_eq!(run("certificate", &s), met("Trusted, valid for 60 more days"));
    s.old[1].1 = tls_ok(60);
    assert_eq!(run("tls-versions", &s), not_met("TLSv1.1 accepted"));
    s.old[1].1 = TlsAnswer::Failed {
        local: true,
        error: "x".into(),
    };
    assert_eq!(
        run("tls-versions", &s),
        unknown("TLSv1.3 accepted; TLSv1.1 couldn't be offered from this computer")
    );
    s.current = TlsAnswer::Ok {
        protocol: "TLSv1.1".into(),
        authorized: true,
        auth_error: None,
        valid_to: None,
    };
    assert_eq!(
        run("tls-versions", &s).detail,
        "The handshake settled on TLSv1.1, not TLS 1.2 or 1.3"
    );
    assert_eq!(
        run("certificate", &s),
        unknown("The certificate's expiry couldn't be read")
    );
    s.current = TlsAnswer::Ok {
        protocol: "TLSv1.3".into(),
        authorized: false,
        auth_error: Some("self signed".into()),
        valid_to: None,
    };
    assert_eq!(
        run("certificate", &s),
        not_met("The certificate isn't trusted: self signed")
    );
    s.current = tls_ok(14);
    assert_eq!(run("certificate", &s), not_met("The certificate expires in 14 days"));
    s.current = tls_ok(-1);
    assert_eq!(run("certificate", &s).detail, "The certificate expires in -1 days");
    s.current = refused();
    assert_eq!(run("tls-versions", &s), error("No TLS handshake: handshake failure"));
    assert_eq!(run("certificate", &s), error("No TLS handshake: handshake failure"));
}

#[test]
fn header_checks_name_the_answers_that_fail() {
    let t = target(false);
    let good = &[
        ("strict-transport-security", "max-age=31536000; includeSubDomains"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer, strict-origin-when-cross-origin"),
        ("content-security-policy", "default-src 'self'; frame-ancestors 'none'"),
        ("permissions-policy", "camera=()"),
    ];
    let bad = &[
        ("strict-transport-security", "max-age=60"),
        ("referrer-policy", "unsafe-url"),
        ("server", "nginx/1.25"),
    ];
    let s = seen(
        &t,
        vec![page("/", resp(200, good, "")), page("/docs", resp(200, bad, ""))],
    );
    assert_eq!(
        run("hsts", &s),
        not_met("No Strict-Transport-Security with max-age of a year or more on: /docs")
    );
    assert_eq!(
        run("nosniff", &s),
        not_met("No X-Content-Type-Options: nosniff on: /docs")
    );
    assert_eq!(
        run("referrer-policy", &s),
        not_met("No Referrer-Policy that keeps the path from other sites on: /docs")
    );
    assert_eq!(
        run("frame-ancestors", &s),
        not_met("No frame-ancestors in a CSP on: /docs")
    );
    assert_eq!(
        run("server-version", &s),
        not_met("A version in Server or X-Powered-By on: /docs")
    );
    assert_eq!(
        run("permissions-policy", &s),
        not_met("No Permissions-Policy on: /docs")
    );
    let only_good = seen(&t, vec![page("/", resp(200, good, ""))]);
    assert_eq!(run("hsts", &only_good), met("On every answer (/)"));
    assert_eq!(status("referrer-policy", &only_good), Status::Met);
    // A redirect isn't an answer to judge.
    let redirected = seen(
        &t,
        vec![page("/", resp(200, good, "")), page("/old", resp(301, &[], ""))],
    );
    assert_eq!(run("hsts", &redirected), met("On every answer (/)"));
}

#[test]
fn csp_must_be_strict_and_trusted_types_asked_for() {
    let t = target(false);
    let strict = "default-src 'none'; script-src 'nonce-abc' 'strict-dynamic'; base-uri 'none'; require-trusted-types-for 'script'";
    let s = seen(
        &t,
        vec![page("/", resp(200, &[("content-security-policy", strict)], ""))],
    );
    assert_eq!(status("csp-strict", &s), Status::Met);
    assert_eq!(status("trusted-types", &s), Status::Met);
    let loose = "default-src 'self'; script-src 'self'; base-uri 'self'";
    let s = seen(
        &t,
        vec![page("/", resp(200, &[("content-security-policy", loose)], ""))],
    );
    assert_eq!(status("csp-strict", &s), Status::NotMet);
    assert_eq!(status("trusted-types", &s), Status::NotMet);
    // object-src falls back to default-src, scripts to default-src.
    let by_default = "default-src 'nonce-x' 'none'; base-uri 'none'";
    let s = seen(
        &t,
        vec![page("/", resp(200, &[("content-security-policy", by_default)], ""))],
    );
    assert_eq!(status("csp-strict", &s), Status::Met);
}

#[test]
fn charset_is_asked_of_text_bodies() {
    let t = target(false);
    let s = seen(
        &t,
        vec![
            page("/", resp(200, &[("content-type", "text/html; charset=utf-8")], "<p>")),
            page("/json", resp(200, &[("content-type", "application/json")], "{}")),
            page("/empty", resp(204, &[], "")),
            page("/svg", resp(200, &[("content-type", "image/svg+xml")], "<svg/>")),
        ],
    );
    assert_eq!(
        run("charset", &s),
        not_met("No Content-Type, or a text one without a charset, on: /svg")
    );
    let s = seen(
        &t,
        vec![
            page("/", resp(200, &[("content-type", "text/plain")], "x")),
            page("/n", resp(200, &[], "x")),
        ],
    );
    assert_eq!(
        run("charset", &s).detail,
        "No Content-Type, or a text one without a charset, on: /, /n"
    );
}

#[test]
fn cookies_are_judged_on_every_answer() {
    let t = target(false);
    let mut a = resp(200, &[], "");
    a.cookies = vec![
        "__Host-sid=1; Path=/; Secure; HttpOnly; SameSite=Lax".to_string(),
        "theme=dark; Path=/".to_string(),
    ];
    let s = seen(&t, vec![page("/", a)]);
    assert_eq!(
        run("cookie-secure", &s),
        not_met("Not Secure, or without a __Host- or __Secure- name: theme")
    );
    assert_eq!(run("cookie-samesite", &s), not_met("No SameSite=Lax or Strict: theme"));
    assert_eq!(run("cookie-host-prefix", &s), not_met("Not named __Host-: theme"));
    assert_eq!(run("cookie-httponly", &s), not_met("Not HttpOnly: theme"));
    let mut b = resp(200, &[], "");
    b.cookies = vec!["__Host-sid=1; Secure; HttpOnly; SameSite=Strict".to_string()];
    let s = seen(&t, vec![page("/", b)]);
    assert_eq!(run("cookie-httponly", &s), met("Every cookie (__Host-sid)"));
    assert_eq!(status("cookie-samesite", &s), Status::Met);
}

#[test]
fn cors_must_not_echo_the_scan_origin() {
    let t = target(false);
    let mut s = seen(&t, vec![]);
    s.cors = Some(resp(200, &[], ""));
    assert_eq!(run("cors-origin", &s), met("https://scan.invalid isn't allowed"));
    s.preflight = Some(resp(
        204,
        &[("access-control-allow-origin", "https://scan.invalid")],
        "",
    ));
    assert_eq!(
        run("cors-origin", &s),
        not_met("Access-Control-Allow-Origin answers https://scan.invalid to a request from https://scan.invalid")
    );
    s.preflight = Some(resp(
        204,
        &[
            ("access-control-allow-origin", "*"),
            ("access-control-allow-credentials", "TRUE"),
        ],
        "",
    ));
    assert_eq!(
        run("cors-origin", &s).detail,
        "Access-Control-Allow-Origin: * with credentials allowed"
    );
    s.preflight = Some(resp(204, &[("access-control-allow-origin", "*")], ""));
    assert_eq!(status("cors-origin", &s), Status::Met);
    let own = target(true);
    let mut s = seen(&own, vec![]);
    s.cors = Some(resp(200, &[("access-control-allow-origin", "*")], ""));
    assert_eq!(
        run("cors-origin", &s).detail,
        "Access-Control-Allow-Origin: * on /api/me, which answers private data"
    );
}

#[test]
fn the_api_must_not_render_as_a_page() {
    let own = target(true);
    let api = |headers: &[(&str, &str)]| Answer {
        resp: resp(401, headers, "{}"),
        kind: Kind::Api,
        label: "the API answer".into(),
    };
    assert_eq!(
        run("api-not-renderable", &seen(&target(false), vec![])).detail,
        "Needs the project's own API routes"
    );
    assert_eq!(
        run("api-not-renderable", &seen(&own, vec![])),
        unknown("/api/me didn't answer")
    );
    assert_eq!(
        run(
            "api-not-renderable",
            &seen(&own, vec![api(&[("content-disposition", " Attachment; filename=x")])])
        ),
        met("Content-Disposition: attachment")
    );
    assert_eq!(
        run(
            "api-not-renderable",
            &seen(&own, vec![api(&[("content-security-policy", "sandbox")])])
        ),
        met("A CSP sandbox")
    );
    let mut s = seen(&own, vec![api(&[])]);
    s.navigated = Some(resp(406, &[], ""));
    assert_eq!(
        run("api-not-renderable", &s),
        met("A request for it as a page (Sec-Fetch-Dest: document) is refused with 406")
    );
    s.navigated = Some(resp(401, &[], ""));
    assert!(
        run("api-not-renderable", &s)
            .detail
            .ends_with("(Sec-Fetch-Dest: document) with 401")
    );
    s.navigated = None;
    assert!(run("api-not-renderable", &s).detail.ends_with("with nothing"));
}

#[test]
fn error_answers_must_not_leak() {
    let t = target(false);
    let nf = |body: &str| Answer {
        resp: resp(404, &[], body),
        kind: Kind::NotFound,
        label: "the 404".into(),
    };
    assert_eq!(status("error-leak", &seen(&t, vec![nf("Not found")])), Status::Met);
    for leak in [
        "Error at Object.run (/x)",
        "at handler (index.js)",
        "Traceback (most recent call last)",
        r"C:\Users\x",
        "see /home/app/server.py",
        "index.ts:42",
    ] {
        let s = seen(&t, vec![nf(leak)]);
        assert_eq!(
            run("error-leak", &s),
            not_met("A stack trace or file path in: the 404"),
            "{leak}"
        );
    }
    // Only a page's own words aren't looked at.
    assert_eq!(
        status(
            "error-leak",
            &seen(&t, vec![page("/", resp(200, &[], "at Object.x ("))])
        ),
        Status::Unknown
    );
}

#[test]
fn signed_in_answers_must_not_be_cached() {
    let mut t = target(false);
    assert!(
        run("signed-in-no-store", &seen(&t, vec![]))
            .detail
            .contains("needs a staging test token")
    );
    t.token = Some("tok".into());
    assert!(
        run("signed-in-no-store", &seen(&t, vec![]))
            .detail
            .contains("needs signed-in paths")
    );
    t.signed_in_paths = vec!["/api/a".into(), "/api/b".into()];
    let signed = |label: &str, status: u16, cc: &str| Answer {
        resp: resp(status, &[("cache-control", cc)], ""),
        kind: Kind::Api,
        label: label.into(),
    };
    let mut s = seen(&t, vec![]);
    assert_eq!(
        run("signed-in-no-store", &s).detail,
        "No signed-in path answered with the test token (no answer)"
    );
    s.signed_in = vec![signed("/api/a", 401, ""), signed("/api/b", 500, "")];
    assert_eq!(
        run("signed-in-no-store", &s).detail,
        "No signed-in path answered with the test token (/api/a 401, /api/b 500)"
    );
    s.signed_in = vec![
        signed("/api/a", 200, "private, No-Store"),
        signed("/api/b", 200, "max-age=60"),
    ];
    assert_eq!(
        run("signed-in-no-store", &s),
        not_met("No Cache-Control: no-store on the signed-in answer of: /api/b")
    );
}

#[test]
fn coop_is_asked_of_html_pages_only() {
    let t = target(false);
    let html = &[("content-type", "text/html; charset=utf-8")];
    let s = seen(
        &t,
        vec![
            page("/", resp(200, html, "")),
            page("/api", resp(200, &[("content-type", "application/json")], "")),
        ],
    );
    assert_eq!(
        run("coop", &s),
        not_met("No Cross-Origin-Opener-Policy: same-origin on: /")
    );
    let with = &[
        ("content-type", "text/html"),
        ("cross-origin-opener-policy", "same-origin-allow-popups"),
    ];
    assert_eq!(
        status("coop", &seen(&t, vec![page("/", resp(200, with, ""))])),
        Status::Met
    );
}

#[test]
fn scripts_from_other_sites_need_integrity() {
    let html = r#"
      <script src="/app.js"></script>
      <script src="https://cdn.other/a.js"></script>
      <script src='//cdn.other/b.js' integrity="sha384-x" crossorigin></script>
      <SCRIPT SRC=https://cdn.other/c.js></SCRIPT>
      <link rel="stylesheet" href="https://fonts.other/f.css">
      <link rel="icon" href="https://cdn.other/i.png">
      <link rel="preload stylesheet" href="https://s.example/own.css">
      <script src="https://s.example/own.js"></script>
      <script>inline()</script>
    "#;
    assert_eq!(
        foreign_without_integrity(html, "s.example"),
        [
            "https://cdn.other/a.js",
            "https://cdn.other/c.js",
            "https://fonts.other/f.css"
        ]
    );
    let t = target(false);
    let h = &[("content-type", "text/html")];
    let s = seen(&t, vec![page("/", resp(200, h, html))]);
    assert_eq!(
        run("sri", &s),
        not_met("Without integrity: https://cdn.other/a.js, https://cdn.other/c.js, https://fonts.other/f.css")
    );
    let s = seen(&t, vec![page("/", resp(200, h, "<script src=\"/a.js\"></script>"))]);
    assert_eq!(status("sri", &s), Status::Met);
}

#[test]
fn security_txt_needs_a_contact_and_a_future_expiry() {
    let t = target(false);
    let txt = &[("content-type", "text/plain; charset=utf-8")];
    let mut s = seen(&t, vec![]);
    let mut check = |status: u16, h: &[(&str, &str)], body: &str| {
        s.security_txt = Some(resp(status, h, body));
        run("security-txt", &s)
    };
    assert_eq!(check(404, &[], ""), not_met("/.well-known/security.txt answers 404"));
    assert_eq!(
        check(200, &[("content-type", "text/html")], ""),
        not_met("/.well-known/security.txt answers 200 text/html")
    );
    assert_eq!(
        check(200, txt, "Expires: 2027-01-01T00:00:00Z"),
        not_met("security.txt has no Contact:")
    );
    assert_eq!(
        check(200, txt, "Contact: mailto:a@b.c"),
        not_met("security.txt has no Expires: in the future")
    );
    assert_eq!(
        check(200, txt, "Contact: mailto:a@b.c\nExpires: 2026-01-01T00:00:00Z"),
        not_met("security.txt has no Expires: in the future")
    );
    assert_eq!(
        check(200, txt, "Contact: mailto:a@b.c\r\nExpires: 2027-01-01T00:00:00.000Z"),
        met("security.txt with a Contact: and an Expires: in the future")
    );
}
