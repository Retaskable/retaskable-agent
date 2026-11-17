use std::cell::Cell;

use super::*;

fn list(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_retaskable_or_claude_branch_is_pushed_to_whoever_opened_it() {
    assert!(may_push_to("retaskable/7-x", None, None, &[], false));
    assert!(may_push_to("retaskable/7-x", Some("stranger"), None, &[], false));
    assert!(may_push_to("claude/fix-it", Some("stranger"), Some("me"), &[], false));
    assert!(!may_push_to("feature/retaskable/x", Some("stranger"), None, &[], false));
    assert!(!may_push_to("Retaskable/7", Some("stranger"), None, &[], false));
}

#[test]
fn the_signed_in_account_and_the_allow_list_match_without_case() {
    assert!(may_push_to("feat/x", Some("Me"), Some("me"), &[], false));
    assert!(may_push_to("feat/x", Some("me"), Some("ME"), &[], false));
    assert!(may_push_to("feat/x", Some("Ann"), None, &list(&["ann"]), false));
    assert!(may_push_to(
        "feat/x",
        Some("ann"),
        Some("me"),
        &list(&["bob", "ANN"]),
        false
    ));
    assert!(!may_push_to("feat/x", Some("ann"), Some("me"), &list(&["bob"]), false));
}

#[test]
fn no_author_or_no_login_is_never_a_match() {
    assert!(!may_push_to("feat/x", None, Some("me"), &list(&["me"]), false));
    assert!(!may_push_to("feat/x", Some(""), Some(""), &list(&[""]), false));
    assert!(!may_push_to("feat/x", Some("me"), None, &[], false));
}

#[test]
fn the_login_is_kept_only_after_a_successful_non_empty_read() {
    let cache = LoginCache::default();
    let reads = Cell::new(0);
    // A failed read and an empty one are read again.
    assert_eq!(
        cache.get(|| {
            reads.set(reads.get() + 1);
            None
        }),
        None
    );
    assert_eq!(
        cache.get(|| {
            reads.set(reads.get() + 1);
            Some("  ".into())
        }),
        None
    );
    assert_eq!(reads.get(), 2);
    assert_eq!(
        cache
            .get(|| {
                reads.set(reads.get() + 1);
                Some("me\n".into())
            })
            .as_deref(),
        Some("me")
    );
    // Read once: the next one does not read it again.
    assert_eq!(cache.get(|| unreachable!("cached")).as_deref(), Some("me"));
    assert_eq!(reads.get(), 3);
}

#[test]
fn a_fork_is_never_pushed_to_whatever_its_branch_or_author() {
    assert!(!may_push_to("retaskable/7-x", None, None, &[], true));
    assert!(!may_push_to("claude/fix-it", Some("me"), Some("me"), &[], true));
    assert!(!may_push_to("feat/x", Some("ann"), Some("me"), &list(&["ann"]), true));
}

#[test]
fn a_row_with_no_is_cross_repository_counts_as_a_fork() {
    let got = parse_open_pulls(r#"[{"number":1,"headRefName":"feat/a","headRefOid":"aaa"}]"#).unwrap();
    assert!(got[0].cross_repository);
}

const PULLS: &str = r#"[
 {"number":1,"title":"Mine","url":"https://github.com/o/r/pull/1","headRefName":"feat/a","headRefOid":"aaa1111","mergeable":"CONFLICTING","author":{"login":"ann"},"isCrossRepository":false},
 {"number":2,"title":"Fork","url":"u2","headRefName":"patch-1","headRefOid":"bbb2222","mergeable":"MERGEABLE","author":{"login":"zed"},"isCrossRepository":true},
 {"number":3,"title":"No head","url":"u3","headRefName":"x","headRefOid":"","mergeable":"UNKNOWN","author":null},
 {"number":4,"title":"Bot","url":"u4","headRefName":"retaskable/4-y","headRefOid":"ddd4444","mergeable":"","author":{"login":""},"isCrossRepository":false,"future":1}
]"#;

#[test]
fn the_list_keeps_what_the_watcher_reads_forks_included_and_skips_headless_ones() {
    let got = parse_open_pulls(PULLS).unwrap();
    assert_eq!(got.iter().map(|p| p.number).collect::<Vec<_>>(), [1, 2, 4]);
    assert_eq!(got[0].head, "aaa1111");
    assert_eq!(got[0].branch, "feat/a");
    assert_eq!(got[0].author.as_deref(), Some("ann"));
    assert!(got[0].conflicting());
    assert_eq!((got[2].author.clone(), got[2].mergeable.clone()), (None, None));
    assert!(!got[2].conflicting());
    assert_eq!(
        (got[1].branch.as_str(), got[1].author.as_deref()),
        ("patch-1", Some("zed"))
    );
    assert_eq!(
        got.iter().map(|p| p.cross_repository).collect::<Vec<_>>(),
        [false, true, false]
    );
    assert!(parse_open_pulls("[]").unwrap().is_empty());
    assert!(parse_open_pulls("nope").is_err());
}

#[cfg(unix)]
#[test]
fn open_pulls_asks_gh_for_the_open_ones_into_the_base() {
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("fake_gh.sh");
    let script = format!(
        "#!/bin/sh\n[ -n \"$1\" ] || exit 0\ncase \"$*\" in\n\"pr list --repo o/r --base main --state open --limit 100 --json number,title,url,headRefName,headRefOid,mergeable,author,isCrossRepository\") cat <<'EOF'\n{PULLS}\nEOF\n;;\n*) echo \"unexpected: $*\" >&2; exit 1;;\nesac\n"
    );
    crate::test_support::write_executable(&gh, &script);
    let got = open_pulls(&gh, dir.path(), "o/r", "main", &|_| {}).unwrap();
    assert_eq!(got.len(), 3);
    let e = open_pulls(&gh, dir.path(), "o/r", "dev", &|_| {})
        .unwrap_err()
        .to_string();
    assert!(e.contains("gh pr list failed") && e.contains("unexpected"), "{e}");
}
