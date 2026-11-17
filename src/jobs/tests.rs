use super::*;

#[test]
fn kinds_are_the_closed_list() {
    for k in JOB_KINDS {
        assert_eq!(JobKind::parse(k).is_some(), !matches!(k, "draft" | "smoke"), "{k}");
    }
    assert_eq!(JobKind::parse("nope"), None);
    assert_eq!(JobKind::parse("release_notes").unwrap().name(), "release_notes");
    assert!(JobKind::Vet.runs_session() && JobKind::Triage.runs_session() && !JobKind::Lighthouse.runs_session());
}

#[test]
fn a_job_spec_reads_the_store_shape() {
    let v = json!({ "kind": "lighthouse", "day": "2026-11-17", "args": { "url": "https://s.example" } });
    let spec = JobSpec::from_json(&v).unwrap();
    assert_eq!((spec.kind.as_str(), spec.day.as_str()), ("lighthouse", "2026-11-17"));
    assert_eq!(spec.arg_str("url"), Some("https://s.example"));
    assert_eq!(JobSpec::from_json(&json!({ "day": "x" })), None);
    assert_eq!(JobSpec::from_json(&json!({ "kind": "vet" })).unwrap().day, "");
}

#[test]
fn can_lists_facts_in_order_and_drops_chrome_once_missing() {
    let chrome = ChromeFlag::default();
    assert_eq!(
        job_can(true, &chrome, true),
        [Can::Chrome, Can::Repo, Can::Engine, Can::ScanToken]
    );
    assert_eq!(job_can(false, &chrome, false), [Can::Chrome]);
    chrome.mark_missing();
    assert_eq!(job_can(true, &chrome, false), [Can::Repo, Can::Engine]);
    assert_eq!(Can::ScanToken.name(), "scanToken");
}

#[test]
fn end_body_has_the_three_shapes() {
    let done: JobOutcome = Outcome::Done {
        result: json!({ "a": 1 }),
        known: BTreeMap::from([("k".to_string(), "d".to_string())]),
    };
    assert_eq!(
        end_body(&done),
        json!({ "ok": true, "result": { "a": 1 }, "known": { "k": "d" } })
    );
    assert_eq!(end_body(&Outcome::done(json!({}))), json!({ "ok": true, "result": {} }));
    assert_eq!(
        end_body(&Outcome::refused("no Chrome")),
        json!({ "ok": false, "refused": { "why": "no Chrome" } })
    );
    let failed: JobOutcome = Outcome::Failed {
        note: "it hit".into(),
        no_retry: true,
        limit_hit: Some(LimitHit {
            engine: Some("claude".into()),
            until: 5,
        }),
    };
    assert_eq!(
        end_body(&failed),
        json!({ "ok": false, "note": "it hit", "noRetry": true, "limitHit": { "engine": "claude", "until": 5 } })
    );
    assert_eq!(end_body(&Outcome::failed("x")), json!({ "ok": false, "note": "x" }));
}

#[test]
fn one_line_squishes_and_cuts() {
    assert_eq!(one_line("  a \n b\t c ", 100), "a b c");
    assert_eq!(one_line("abcdef", 3), "abc");
    assert_eq!(one_line("ab  cd", 3), "ab");
    assert_eq!(one_line("", 3), "");
}

#[test]
fn same_origin_compares_scheme_host_and_port() {
    assert!(same_origin("https://s.example/a", "https://S.example"));
    assert!(same_origin("https://s.example:443/", "https://s.example"));
    assert!(!same_origin("https://s.example", "http://s.example"));
    assert!(!same_origin("https://s.example", "https://evil.example"));
    assert!(!same_origin("https://s.example:8443", "https://s.example"));
    assert!(!same_origin("nope", "https://s.example"));
    assert!(!same_origin("nope", "nope"));
    assert_eq!(origin_of("ftp://x"), None);
}

#[test]
fn the_unknown_kind_and_failure_words() {
    assert_eq!(
        unknown_kind("draft"),
        Outcome::Refused("This computer doesn't run draft jobs.".into())
    );
    assert_eq!(
        run_failed::<()>("vet", &"boom"),
        Outcome::failed("The vet run failed: boom")
    );
}
