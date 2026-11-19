//! What the store adds to a fix or merge (`tier`, `prior`), what a session says back (`did.summary`) and `waitMain`.
//! Hand-made answers: the store's fixtures (tests/fixtures/store/answers.json) have none of these fields until the store
//! PR lands, and are to be regenerated then.
use super::fix::{acts, commits, pushes};
use super::merge::{BRANCH, list_and_green, pull_by};
use super::*;
use crate::backlog::ci_result::{CiPrior, CiTier};

fn prior(n: u64) -> Value {
    json!({ "at": 1_763_500_000_000_u64 + n, "kind": "fix", "pushed": true, "summary": format!("attempt {n}"),
        "ci": { "sha": "abcdef0123", "excerpt": format!("not ok {n}") } })
}

/// The store says `next` on a head's first answer, then is green.
fn says(rig: &Rig, next: Value) {
    rig.store.answers(move |a| {
        if a["did"].is_null() {
            tell(next.clone())
        } else {
            green()
        }
    });
}

async fn run_fix(rig: &Rig, next: Value) -> Vec<Value> {
    list_and_green(rig, vec![rig.at_tip(pull_by(9, BRANCH, "stranger", "MERGEABLE"))]);
    says(rig, next);
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    rig.store.sent("ci_result")
}

#[tokio::test]
async fn a_strong_fix_carries_its_tier_and_at_most_five_earlier_attempts_and_sends_the_sessions_summary() {
    let mut rig = Rig::new().await;
    rig.with_repo();
    acts(&rig, commits);
    *rig.fixer.says.lock().unwrap() = Some("Fixed the import; checks passed.".into());
    let many: Vec<Value> = (1..=7).map(prior).collect();
    let sent = run_fix(&rig, json!({ "do": "fix", "tier": "strong", "prior": many })).await;
    let ctx = rig.fixer.contexts.lock().unwrap().clone();
    assert_eq!(ctx.len(), 1);
    assert_eq!(ctx[0].tier, CiTier::Strong);
    assert_eq!(ctx[0].prior.len(), 5, "the store sends at most 5, and no more are read");
    assert_eq!(ctx[0].prior[0].ci.as_ref().unwrap().excerpt, "not ok 1");
    let did = &sent[1]["did"];
    assert_eq!(did["kind"], "fix");
    assert_eq!(did["summary"], "Fixed the import; checks passed. Pushed.");
    assert_eq!(did["pushed"], true);
}

#[tokio::test]
async fn no_tier_or_prior_is_a_normal_fix_and_a_long_summary_is_cut_to_1000_units() {
    let mut rig = Rig::new().await;
    rig.with_repo();
    acts(&rig, |_| true);
    *rig.fixer.says.lock().unwrap() = Some("é𝄞".repeat(700));
    let sent = run_fix(&rig, json!({ "do": "fix" })).await;
    let ctx = rig.fixer.contexts.lock().unwrap().clone();
    assert_eq!((ctx[0].tier, ctx[0].prior.len()), (CiTier::Normal, 0));
    let summary = sent[1]["did"]["summary"].as_str().unwrap();
    assert!(
        summary.encode_utf16().count() <= 1000,
        "{}",
        summary.encode_utf16().count()
    );
    assert!(sent[1]["did"]["pushed"] == false && summary.starts_with('é'));
}

#[tokio::test]
async fn a_session_that_says_nothing_sends_no_summary() {
    let mut rig = Rig::new().await;
    rig.with_repo();
    let sent = run_fix(&rig, json!({ "do": "fix" })).await;
    assert!(sent[1]["did"].get("summary").is_none(), "{:?}", sent[1]["did"]);
}

#[tokio::test]
async fn wait_main_runs_no_fix_and_says_nothing_back_so_no_attempt_is_burned() {
    let mut rig = Rig::new().await;
    rig.with_repo();
    let sent = run_fix(
        &rig,
        json!({ "do": "waitMain", "main": { "sha": "abc1234", "excerpt": "main: build is red" } }),
    )
    .await;
    assert_eq!(sent.len(), 1, "no `did` follows: {sent:?}");
    assert!(rig.fixer.asked.lock().unwrap().is_empty());
    assert!(pushes(&rig).is_empty());
    assert!(!rig.fixing.holds(9, &rig.tip));
}

#[tokio::test]
async fn a_red_with_no_readable_log_gets_the_latest_ci_comment_in_the_fix_note() {
    let mut rig = Rig::new().await;
    rig.with_repo();
    let tip = rig.tip.clone();
    // A failing check whose link is not an Actions run: no log can be read from it.
    let mut run = run_json("build", "completed", Some("failure"));
    run["html_url"] = json!("https://ci.example/job/1");
    head_is(rig.dir.path(), &tip, vec![run]);
    list(
        rig.dir.path(),
        vec![rig.at_tip(pull_by(9, BRANCH, "stranger", "MERGEABLE"))],
    );
    let body =
        serde_json::to_string("agent-ci box failed on abc at step: test.\n\n```\nnot ok 3 - parse\n```\n").unwrap();
    std::fs::write(rig.dir.path().join("comments.out"), format!("{body}\n")).unwrap();
    says(&rig, json!({ "do": "fix" }));
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    let asked = rig.fixer.asked.lock().unwrap().clone();
    assert!(asked[0].3.text().contains("not ok 3 - parse"), "{:?}", asked[0].3);
    assert!(asked[0].3.text().contains("latest CI comment"));
}

#[test]
fn prior_attempts_parse_from_the_stores_words() {
    let p: CiPrior = serde_json::from_value(prior(2)).unwrap();
    assert_eq!(
        (p.kind.as_str(), p.pushed, p.summary.as_str()),
        ("fix", true, "attempt 2")
    );
}

#[tokio::test]
async fn a_fix_waits_while_the_base_branchs_own_ci_is_red_and_says_nothing() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    // Main's head has a failing check of its own.
    let main = super::merge::rev(&origin, "main");
    super::red(rig.dir.path(), &main);
    let sent = run_fix(&rig, json!({ "do": "fix" })).await;
    assert_eq!(sent.len(), 1, "no `did`, so no attempt is burned: {sent:?}");
    assert!(rig.fixer.asked.lock().unwrap().is_empty());
    assert!(pushes(&rig).is_empty());
    assert!(!rig.fixing.holds(9, &rig.tip));
}

#[tokio::test]
async fn the_stores_ci_for_the_red_run_is_the_sessions_note_and_no_comment_is_fetched() {
    let mut rig = Rig::new().await;
    rig.with_repo();
    let next = json!({ "do": "fix", "ci": { "sha": "abc", "excerpt": "build: tsc failed\nsrc/a.ts(3): bad", "link": "https://github.com/o/r/pull/9#c1" } });
    run_fix(&rig, next).await;
    let asked = rig.fixer.asked.lock().unwrap().clone();
    let note = asked[0].3.text();
    assert!(
        note.contains("build: tsc failed") && note.contains("https://github.com/o/r/pull/9#c1"),
        "{note}"
    );
    assert!(
        !rig.gh_log().iter().any(|l| l.contains("issues/9/comments")),
        "nothing fetched itself"
    );
}

#[tokio::test]
async fn every_ci_result_carries_the_base_head_not_only_a_conflicted_heads() {
    let mut rig = Rig::new().await;
    let origin = rig.with_repo();
    let sent = run_fix(&rig, json!({ "do": "fix" })).await;
    let main = super::merge::rev(&origin, "main");
    assert!(sent.iter().all(|f| f["baseHead"] == main), "{sent:?}");
    // A green head the computer may not push to as well.
    let rig2 = Rig::new().await;
    let mut rig2 = rig2;
    rig2.with_repo();
    list_and_green(&rig2, vec![rig2.at_tip(pull_by(9, BRANCH, "stranger", "MERGEABLE"))]);
    rig2.store.answers(|_| green());
    rig2.pass(&mut HashMap::new(), 0).await.unwrap();
    assert!(rig2.store.sent("ci_result")[0]["baseHead"].is_string());
}
