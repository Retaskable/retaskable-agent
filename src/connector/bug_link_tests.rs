use super::*;
use crate::release::Environment;

fn link() -> BugLink {
    BugLink::new(Duration::from_secs(1))
}

#[test]
fn consent_said_lists_known_bugs_only_while_on_and_skips_ones_without_an_id() {
    let l = link();
    l.consent_said(&json!({
        "on": true,
        "known": [{ "id": "b1", "title": "One" }, { "id": "", "title": "No id" }, { "title": "Missing" }, { "id": "b2" }],
    }));
    assert!(l.consent());
    let known = l.known();
    assert_eq!(known.len(), 2);
    assert_eq!((known[0].id.as_str(), known[0].title.as_str()), ("b1", "One"));
    assert_eq!((known[1].id.as_str(), known[1].title.as_str()), ("b2", ""));

    l.consent_said(&json!({ "on": false, "known": [{ "id": "b1", "title": "One" }] }));
    assert!(!l.consent());
    assert!(l.known().is_empty());

    // A message with no `on` at all is off.
    l.consent_said(&json!({ "on": true }));
    l.consent_said(&json!({ "known": [{ "id": "b1" }] }));
    assert!(!l.consent());
    assert!(l.known().is_empty());
}

#[test]
fn closed_turns_consent_off_but_keeps_counting_refs() {
    let l = link();
    l.consent_said(&json!({ "on": true }));
    l.state().sent = 7;
    let (tx, _rx) = mpsc::unbounded_channel();
    l.opened(tx);
    l.closed();
    let state = l.state();
    assert!(!state.on);
    assert!(state.out.is_none());
    assert!(state.waiters.is_empty());
    assert_eq!(state.sent, 7);
}

#[test]
fn an_answer_without_a_ref_or_for_nobody_is_ignored() {
    let l = link();
    l.answered(&json!({ "ok": true }));
    l.answered(&json!({ "ref": "bug-9", "ok": true }));
    assert!(l.state().waiters.is_empty());
}

#[test]
fn an_answer_defaults_missing_fields_and_reaches_its_waiter() {
    let l = link();
    let (tx, mut rx) = oneshot::channel();
    l.state().waiters.insert("bug-1".into(), tx);
    l.answered(&json!({ "ref": "bug-1" }));
    assert_eq!(
        rx.try_recv().unwrap(),
        BugReported {
            ok: false,
            duplicate: false,
            reason: None,
            item_id: None
        }
    );
    assert!(l.state().waiters.is_empty());
}

#[test]
fn bug_version_for_a_release_build_is_its_version_and_commit() {
    let build = Build {
        os: "linux".into(),
        environment: Environment::Production,
        version: "1.2.3".into(),
        commit: "abc1234".into(),
    };
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(bug_version(Some(&build), "ignored", dir.path()), "1.2.3 (abc1234)");
}

#[test]
fn bug_version_in_a_checkout_adds_the_short_commit_or_falls_back_to_the_version() {
    let dir = tempfile::tempdir().unwrap();
    // Not a git checkout (and a missing directory): just the version.
    assert_eq!(bug_version(None, "0.9", dir.path()), "0.9");
    assert_eq!(bug_version(None, "0.9", &dir.path().join("missing")), "0.9");

    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.com",
        "-c",
        "commit.gpgsign=false",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "x",
    ]);
    let sha = git(&["rev-parse", "--short", "HEAD"]);
    assert!(!sha.is_empty());
    assert_eq!(bug_version(None, "0.9", dir.path()), format!("0.9 ({sha})"));
}
