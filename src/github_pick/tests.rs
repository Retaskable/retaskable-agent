use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

fn out(code: i32, stdout: &str, stderr: &str) -> Result<Out, String> {
    Ok(Out {
        code,
        stdout: stdout.into(),
        stderr: stderr.into(),
    })
}

/// A `gh` that answers by its arguments and counts its calls; time moves when `skip` says.
struct Rig {
    picker: GithubPicker,
    calls: Arc<AtomicUsize>,
    skip: Arc<Mutex<Duration>>,
    answers: Arc<Mutex<HashMap<&'static str, Result<Out, String>>>>,
}

fn rig() -> Rig {
    let calls = Arc::new(AtomicUsize::new(0));
    let skip = Arc::new(Mutex::new(Duration::ZERO));
    let answers: Arc<Mutex<HashMap<&'static str, Result<Out, String>>>> = Arc::default();
    {
        let mut a = answers.lock().unwrap();
        a.insert("user", out(0, r#"{"login":"Me"}"#, ""));
        a.insert("orgs", out(0, "zeta\nAlpha\nzeta\nbad name\n", ""));
        a.insert(
            "repos",
            out(
                0,
                r#"[{"nameWithOwner":"Me/old","description":"  ","visibility":"PUBLIC","pushedAt":"2026-01-01T00:00:00Z","isFork":false},
                    {"nameWithOwner":"Me/new","description":" A thing ","visibility":"PRIVATE","pushedAt":"2026-06-01T00:00:00Z","isFork":true},
                    {"nameWithOwner":"Me/never","visibility":"INTERNAL"},
                    {"description":"no name"}]"#,
                "",
            ),
        );
    }
    let (c, a) = (calls.clone(), answers.clone());
    let base = Instant::now();
    let s = skip.clone();
    let picker = GithubPicker::with(
        Arc::new(move |args: &[&str]| {
            c.fetch_add(1, Ordering::SeqCst);
            let key = match args {
                ["api", "user"] => "user",
                ["api", "--paginate", ..] => "orgs",
                ["repo", "list", ..] => "repos",
                other => panic!("unexpected gh call {other:?}"),
            };
            a.lock().unwrap().get(key).cloned().unwrap()
        }),
        Arc::new(move || base + *s.lock().unwrap()),
    );
    Rig {
        picker,
        calls,
        skip,
        answers,
    }
}

#[test]
fn owners_are_your_login_then_your_organisations_sorted_with_no_repeats_or_odd_names() {
    let r = rig();
    assert_eq!(
        r.picker.owners(false),
        json!({ "login": "Me", "orgs": ["Alpha", "zeta"] })
    );
}

#[test]
fn a_gh_that_is_signed_out_says_how_to_sign_in_and_is_not_kept() {
    let r = rig();
    r.answers.lock().unwrap().insert(
        "user",
        out(1, "", "To get started with GitHub CLI, please run:  gh auth login\n"),
    );
    assert_eq!(
        r.picker.owners(false),
        json!({ "login": null, "orgs": [], "error": "gh isn't signed in: run `gh auth login`" })
    );
    r.answers
        .lock()
        .unwrap()
        .insert("user", out(0, r#"{"login":"Me"}"#, ""));
    assert_eq!(r.picker.owners(false)["login"], "Me", "a failure isn't kept");
}

#[test]
fn another_failure_gives_its_first_line_and_a_gh_that_will_not_start_says_so() {
    let r = rig();
    r.answers
        .lock()
        .unwrap()
        .insert("user", out(1, "", "\n  HTTP 500: boom\nmore\n"));
    assert_eq!(r.picker.owners(false)["error"], "HTTP 500: boom");
    r.answers
        .lock()
        .unwrap()
        .insert("user", Err("Couldn't run gh: not found".into()));
    assert_eq!(r.picker.owners(false)["error"], "Couldn't run gh: not found");
    r.answers.lock().unwrap().insert("user", out(0, "{}", ""));
    assert_eq!(r.picker.owners(false)["error"], "gh api user answered without a login");
    r.answers.lock().unwrap().insert("user", out(0, "nope", ""));
    assert_eq!(
        r.picker.owners(false)["error"],
        "gh api user answered something that isn't JSON"
    );
}

#[test]
fn a_token_without_the_org_scope_still_has_its_login_and_says_the_scope_is_missing() {
    let r = rig();
    r.answers
        .lock()
        .unwrap()
        .insert("orgs", out(1, "", "gh: Resource not accessible (HTTP 403)\n"));
    assert_eq!(
        r.picker.owners(false),
        json!({ "login": "Me", "orgs": [], "scopeMissing": true })
    );
    r.answers
        .lock()
        .unwrap()
        .insert("orgs", out(1, "", "dial tcp: timeout\n"));
    r.picker.owners(true);
    assert_eq!(
        r.picker.owners(true),
        json!({ "login": "Me", "orgs": [], "error": "dial tcp: timeout" })
    );
}

#[test]
fn owners_are_kept_five_minutes_and_fresh_asks_again() {
    let r = rig();
    r.picker.owners(false);
    let one = r.calls.load(Ordering::SeqCst);
    assert_eq!(one, 2);
    *r.skip.lock().unwrap() = Duration::from_secs(299);
    r.picker.owners(false);
    assert_eq!(r.calls.load(Ordering::SeqCst), one, "kept");
    *r.skip.lock().unwrap() = Duration::from_secs(301);
    r.picker.owners(false);
    assert_eq!(r.calls.load(Ordering::SeqCst), one * 2, "expired");
    r.picker.owners(true);
    assert_eq!(r.calls.load(Ordering::SeqCst), one * 3, "fresh");
}

#[test]
fn repos_are_newest_push_first_with_what_is_linked_and_the_rows_without_a_name_dropped() {
    let r = rig();
    let repos = r.picker.repos("me", false, &["me/NEW".to_string()]).unwrap();
    assert_eq!(
        repos,
        json!([
            { "nameWithOwner": "Me/new", "description": "A thing", "private": true,
              "pushedAt": "2026-06-01T00:00:00Z", "fork": true, "linked": true },
            { "nameWithOwner": "Me/old", "description": null, "private": false,
              "pushedAt": "2026-01-01T00:00:00Z", "fork": false, "linked": false },
            { "nameWithOwner": "Me/never", "description": null, "private": true,
              "pushedAt": null, "fork": false, "linked": false },
        ])
    );
}

#[test]
fn an_owner_not_listed_is_refused_before_gh_is_asked_for_its_repos() {
    let r = rig();
    assert_eq!(
        r.picker.repos("stranger", false, &[]).unwrap_err(),
        "stranger isn't you or one of your GitHub organisations."
    );
    // The organisation's name in any case is its listed spelling.
    r.picker.repos("ALPHA", false, &[]).unwrap();
}

#[test]
fn repos_are_kept_per_owner_and_a_failed_listing_is_not() {
    let r = rig();
    r.picker.repos("Me", false, &[]).unwrap();
    let calls = r.calls.load(Ordering::SeqCst);
    r.picker.repos("me", false, &["x/y".into()]).unwrap();
    assert_eq!(r.calls.load(Ordering::SeqCst), calls, "kept, whatever the case");
    r.picker.repos("me", true, &[]).unwrap();
    assert_eq!(r.calls.load(Ordering::SeqCst), calls + 1);
    // Fresh owners forget the repo lists.
    r.picker.owners(true);
    let before = r.calls.load(Ordering::SeqCst);
    r.picker.repos("me", false, &[]).unwrap();
    assert_eq!(r.calls.load(Ordering::SeqCst), before + 1);
    r.answers
        .lock()
        .unwrap()
        .insert("repos", out(1, "", "HTTP 502: bad gateway\n"));
    let why = r.picker.repos("Alpha", false, &[]).unwrap_err();
    assert_eq!(why, "Couldn't list Alpha's repos: HTTP 502: bad gateway");
    r.answers.lock().unwrap().insert("repos", out(0, "[]", ""));
    assert_eq!(r.picker.repos("Alpha", false, &[]).unwrap(), json!([]));
}
