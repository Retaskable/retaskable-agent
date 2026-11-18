use super::*;
use std::cell::RefCell;

fn ok(etag: Option<&str>, body: &str) -> Answer {
    Answer {
        status: 200,
        etag: etag.map(String::from),
        body: body.into(),
    }
}

fn not_modified() -> Answer {
    Answer {
        status: 304,
        etag: None,
        body: String::new(),
    }
}

#[test]
fn a_304_returns_the_last_answer_and_the_etag_is_sent() {
    let cache = Cache::default();
    let sent = RefCell::new(Vec::new());
    let get = |answer: Answer| {
        cache
            .get("u", |_, etag| {
                sent.borrow_mut().push(etag.map(String::from));
                Ok(answer)
            })
            .unwrap()
    };
    assert_eq!(get(ok(Some("\"a\""), "one")).body, "one");
    assert_eq!(get(not_modified()).body, "one");
    assert_eq!(get(ok(Some("\"b\""), "two")).body, "two");
    assert_eq!(get(not_modified()).body, "two");
    assert_eq!(
        *sent.borrow(),
        [None, Some("\"a\"".into()), Some("\"a\"".into()), Some("\"b\"".into())]
    );
}

#[test]
fn an_answer_without_an_etag_is_not_cached_and_a_stray_304_passes_through() {
    let cache = Cache::default();
    cache.get("u", |_, _| Ok(ok(None, "x"))).unwrap();
    let r = cache
        .get("u", |_, etag| {
            assert!(etag.is_none());
            Ok(not_modified())
        })
        .unwrap();
    assert_eq!(r.status, 304);
    assert!(cache.get("u", |_, _| Err(anyhow::anyhow!("down"))).is_err());
}

#[test]
fn gh_api_include_output_is_read() {
    let a = parse_include("HTTP/2.0 200 OK\r\nContent-Type: x\r\nEtag: W/\"abc\"\r\n\r\n[1]\n");
    assert_eq!(
        (a.status, a.etag.as_deref(), a.body.as_str()),
        (200, Some("W/\"abc\""), "[1]\n")
    );
    let n = parse_include("HTTP/2.0 304 Not Modified\r\nEtag: \"abc\"\r\n\r\n");
    assert_eq!((n.status, n.body.as_str()), (304, ""));
    let plain = parse_include("[]");
    assert_eq!((plain.status, plain.etag, plain.body.as_str()), (200, None, "[]"));
}

#[cfg(unix)]
#[test]
fn a_host_get_sends_the_etag_back_and_a_304_is_the_cached_body() {
    let d = tempfile::tempdir().unwrap();
    let program = d.path().join("gh");
    crate::test_support::write_executable(
        &program,
        "#!/bin/sh\n[ -n \"$1\" ] || exit 0\necho \"$*\" >> calls.log\ncase \"$*\" in\n*if-none-match*) printf 'HTTP/2.0 304 Not Modified\\r\\n\\r\\n'; exit 1;;\n\
         *) printf 'HTTP/2.0 200 OK\\r\\nEtag: \"v1\"\\r\\n\\r\\n[\"body\"]';;\nesac\n",
    );
    let get = || get_cached_in(program.as_os_str(), d.path(), "etag/test-repo", "repos/x/statuses").unwrap();
    assert_eq!(get(), "[\"body\"]");
    assert_eq!(get(), "[\"body\"]");
    let calls = std::fs::read_to_string(d.path().join("calls.log")).unwrap();
    assert_eq!(calls.lines().count(), 2);
    assert!(
        calls.lines().nth(1).unwrap().contains("if-none-match: \"v1\""),
        "{calls}"
    );
}

fn leaked(limit: u64, remaining: u64) -> &'static Budget {
    let b: &'static Budget = Box::leak(Box::default());
    let reset = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    b.record(limit, remaining, reset);
    b
}

#[test]
fn a_low_allowance_reuses_the_held_answer_without_a_request() {
    let cache = Cache::with_budget(leaked(5000, 500));
    cache.get("u", |_, _| Ok(ok(Some("\"a\""), "one"))).unwrap();
    let r = cache
        .get("u", |_, _| panic!("a low allowance asks for nothing it already has"))
        .unwrap();
    assert_eq!(r.body, "one");
}

#[test]
fn a_healthy_allowance_asks_every_time() {
    let cache = Cache::with_budget(leaked(5000, 4500));
    cache.get("u", |_, _| Ok(ok(Some("\"a\""), "one"))).unwrap();
    let asked = RefCell::new(0);
    cache
        .get("u", |_, etag| {
            *asked.borrow_mut() += 1;
            assert_eq!(etag, Some("\"a\""));
            Ok(not_modified())
        })
        .unwrap();
    assert_eq!(*asked.borrow(), 1);
}

#[test]
fn a_nearly_spent_allowance_asks_for_nothing_new() {
    let cache = Cache::with_budget(leaked(5000, 10));
    let r = cache.get("new", |_, _| {
        panic!("nothing new is asked while the allowance is nearly spent")
    });
    assert!(r.is_err());
}

#[test]
fn the_cache_does_not_grow_without_end() {
    let cache = Cache::default();
    for n in 0..KEPT_MAX + 5 {
        cache.get(&format!("u{n}"), |_, _| Ok(ok(Some("\"e\""), "x"))).unwrap();
    }
    assert!(cache.seen.lock().unwrap().len() <= KEPT_MAX);
}
