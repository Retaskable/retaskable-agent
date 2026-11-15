//! A rerun on the code host: what `gh` is told for `force` and `stuck`, and for the `ci-ready` label.
use super::*;

fn checks(dir: &Path, pr: u64, rows: &[(&str, &str, u64)]) {
    let rows: Vec<Value> = rows
        .iter()
        .map(|(name, bucket, run)| {
            json!({ "name": name, "bucket": bucket, "link": format!("https://github.com/o/r/actions/runs/{run}/job/1") })
        })
        .collect();
    std::fs::write(dir.join(format!("checks-{pr}.json")), Value::Array(rows).to_string()).unwrap();
}

async fn rerun_asked(rig: &Rig, next: Value) -> Value {
    list(rig.dir.path(), vec![pull(7, "feat/x", "eee")]);
    red(rig.dir.path(), "eee");
    rig.store.answers(move |a| {
        if a["did"].is_null() {
            tell(next.clone())
        } else {
            green()
        }
    });
    rig.pass(&mut HashMap::new(), 0).await.unwrap();
    rig.store.sent("ci_result")[1]["did"].clone()
}

#[tokio::test]
async fn force_runs_the_failed_and_cancelled_runs_again() {
    let rig = Rig::new().await;
    checks(
        rig.dir.path(),
        7,
        &[
            ("a", "fail", 45),
            ("b", "cancel", 46),
            ("c", "fail", 45),
            ("d", "pass", 47),
            ("e", "pending", 48),
        ],
    );
    let did = rerun_asked(&rig, json!({ "do": "rerun", "rerun": "force", "hung": false })).await;
    assert_eq!(did, json!({ "kind": "rerun", "rerun": "force", "ok": true }));
    let runs = rig.run_calls();
    assert_eq!(
        runs,
        ["run rerun 45 --failed --repo o/r", "run rerun 46 --failed --repo o/r"]
    );
}

#[tokio::test]
async fn stuck_cancels_the_pending_runs_first_and_then_runs_the_cancelled_ones() {
    let rig = Rig::new().await;
    // A check of another CI has no run behind it.
    std::fs::write(
        rig.dir.path().join("checks-7.json"),
        json!([
            { "name": "a", "bucket": "pending", "link": "https://github.com/o/r/actions/runs/43/job/1" },
            { "name": "b", "bucket": "cancel", "link": "https://github.com/o/r/actions/runs/44/job/1" },
            { "name": "o", "bucket": "pending", "link": "https://ci.example/build/9" }
        ])
        .to_string(),
    )
    .unwrap();
    let did = rerun_asked(&rig, json!({ "do": "rerun", "rerun": "stuck", "hung": true })).await;
    assert_eq!(did, json!({ "kind": "rerun", "rerun": "stuck", "ok": true }));
    let runs = rig.run_calls();
    assert_eq!(runs, ["run cancel 43 --repo o/r", "run rerun 44 --failed --repo o/r"]);
}

#[tokio::test]
async fn a_rerun_with_no_run_behind_the_checks_didnt_go() {
    let rig = Rig::new().await;
    checks(rig.dir.path(), 7, &[("pass", "pass", 1)]);
    let did = rerun_asked(&rig, json!({ "do": "rerun", "rerun": "force" })).await;
    assert_eq!(did, json!({ "kind": "rerun", "rerun": "force", "ok": false }));
}

#[tokio::test]
async fn a_rerun_that_went_forgets_when_the_head_was_first_pending() {
    let rig = Rig::new().await;
    checks(rig.dir.path(), 7, &[("a", "cancel", 44)]);
    list(rig.dir.path(), vec![pull(7, "feat/x", "eee")]);
    head_is(rig.dir.path(), "eee", vec![run_json("build", "in_progress", None)]);
    rig.store.answers(|a| {
        if a["did"].is_null() {
            tell(json!({ "do": "rerun", "rerun": "stuck", "hung": true }))
        } else {
            green()
        }
    });
    let mut since = HashMap::new();
    rig.pass(&mut since, 3_000).await.unwrap();
    assert!(since.is_empty());
}

fn label_calls(rig: &Rig) -> Vec<String> {
    rig.gh_log()
        .into_iter()
        .filter(|l| l.contains("labels") || l.starts_with("label "))
        .collect()
}

#[tokio::test]
async fn with_ci_when_ready_a_rerun_takes_the_label_off_and_puts_it_back() {
    let mut rig = Rig::new().await;
    rig.ready_label = true;
    let did = rerun_asked(&rig, json!({ "do": "rerun", "rerun": "force", "hung": true })).await;
    assert_eq!(did, json!({ "kind": "rerun", "rerun": "force", "ok": true }));
    assert_eq!(
        label_calls(&rig),
        [
            "api -X DELETE repos/o/r/issues/7/labels/ci-ready",
            "api -X POST repos/o/r/issues/7/labels -f labels[]=ci-ready"
        ]
    );
    assert!(rig.run_calls().is_empty(), "no gh run rerun: {:?}", rig.gh_log());
}

#[tokio::test]
async fn a_repository_with_no_such_label_gets_it_made_and_the_label_added_again() {
    let mut rig = Rig::new().await;
    rig.ready_label = true;
    std::fs::write(rig.dir.path().join("no-label"), "").unwrap();
    let did = rerun_asked(&rig, json!({ "do": "rerun", "rerun": "force" })).await;
    assert_eq!(did["ok"], true);
    let calls = label_calls(&rig);
    assert_eq!(calls.len(), 4, "{calls:?}");
    assert!(
        calls[2].starts_with("label create ci-ready --repo o/r --force"),
        "{calls:?}"
    );
    assert_eq!(calls[1], calls[3]);
}
