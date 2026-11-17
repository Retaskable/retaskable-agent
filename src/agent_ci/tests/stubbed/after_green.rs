//! docs/specs/agent-ci.md section 3.3: a green check asks the store at once and merges only what it asks for.
use super::*;

/// Polls while the store asks for nothing, then lets it ask (or not) and runs the pull request's check to its end.
async fn check_then_ask(ask: bool) -> Rig {
    let mut r = green_rig();
    // The statuses are empty until the check posts its success, as for a head not yet checked.
    let log = r._dir.path().join("calls.log");
    let ok = r#"[{"context":"ci/agent/pr/ci","state":"success","description":"agent-ci t: passed in 1m","updated_at":"2026-01-01T00:00:00Z"}]"#;
    script(
        &r._dir.path().join("bin"),
        "gh",
        &format!(
            "echo \"gh $*\" >> {l}\nif [ \"$1\" = api ]; then\n case \"$4\" in\n repos/o/p/pulls/7) echo '{LABELLED}';;\n \
             *commits/*/statuses*) if grep -q state=success {l}; then echo '{ok}'; else echo '[]'; fi;;\n *) [ \"$3\" = GET ] && echo '[]';;\n esac\n exit 0\nfi\n\
             echo '[{{\"number\":7,\"isDraft\":false,\"labels\":[{{\"name\":\"automerge\"}}],\"headRefOid\":\"h7\",\"headRefName\":\"fix\",\"latestReviews\":[{{\"state\":\"APPROVED\"}}]}}]'",
            l = log.display()
        ),
    );
    let keys = r.ci.poll(&r.cfg).await;
    assert!(!calls(&r).contains("/merge"));
    if ask {
        r.store.ask(vec![
            serde_json::json!({ "pr": 7, "sha": "h7", "by": "automerge", "merge": true }),
        ]);
    }
    let pr: Vec<_> = keys["o-p"].iter().filter(|k| !k.is_push()).cloned().collect();
    for t in r.ci.start(&r.cfg, [("o-p".to_string(), pr)].into()).await {
        t.await.unwrap();
    }
    r
}

#[tokio::test]
async fn a_green_check_asks_the_store_at_once_and_merges_when_it_says_yes() {
    let r = check_then_ask(true).await;
    assert!(
        calls(&r).contains("-X PUT repos/o/p/pulls/7/merge -f merge_method=squash -f sha=h7"),
        "{}",
        calls(&r)
    );
    assert_eq!(r.store.args("ci_merged").len(), 1);
}

#[tokio::test]
async fn a_green_check_merges_nothing_when_the_store_answers_nothing() {
    let r = check_then_ask(false).await;
    // It asked (the poll's and the check's), was not asked to merge, and merged nothing.
    assert_eq!(r.store.args("ci_merge_requests").len(), 2);
    assert!(!calls(&r).contains("/merge"), "{}", calls(&r));
    assert!(r.store.args("ci_merged").is_empty());
}
