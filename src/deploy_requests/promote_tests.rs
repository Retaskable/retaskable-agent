//! The `promote` request through [`Live`]: what it takes to claim one, what it refuses in its own words, and the deploy
//! log row it leaves for the store (real temp repos with a bare origin, a fake `gh`, a fake staging site and store).
use serde_json::{Value, json};

use super::tests::{live, request};
use super::*;
use crate::backlog::deploy::DeployKind;
use crate::config::{DeployConfig, DeployTarget};
use crate::promote::tests::fixture::{Fx, staging};

fn target(branch: &str, url: Option<&str>) -> Option<DeployTarget> {
    Some(DeployTarget {
        branch: branch.into(),
        url: url.map(String::from),
        smoke: None,
        version_path: None,
        rollout: None,
    })
}

fn promote_request(sha: Option<&str>) -> DeployRequest {
    let mut r = request("dr-1", DeployKind::Promote);
    r.args.sha = sha.map(String::from);
    r.issued_by = "ann".into();
    r
}

/// A live loop over `fx`, its staging serving `served` and its `gh` knowing ann as an admin.
async fn over(fx: &Fx, base: &str, served: &str) -> Live {
    let url = staging(200, &format!("{{\"sha\":\"{served}\"}}")).await;
    let mut l = live(base, Some("tok"));
    l.project.repo_path = fx.work.to_string_lossy().into_owned();
    l.project.github_repo = "o/r".into();
    l.project.deploy = DeployConfig {
        merges_to: None,
        staging: target("staging", Some(&url)),
        production: target("production", None),
    };
    l.project_dir = fx.dir();
    l.gh = fx.gh("machine", Some((true, true)), &[("ann", "admin")]);
    l
}

#[tokio::test]
async fn a_promote_is_claimed_only_with_both_branches_merges_to_staging_and_a_project_that_is_not_paused() {
    let fx = Fx::new();
    let (base, _) = crate::backlog::tests::store(200, json!({ "value": [] })).await;
    let ok = over(&fx, &base, &fx.c3).await;
    let r = promote_request(Some(&fx.c3));
    assert!(ok.can(&r).await);
    // Not even a sha is needed to claim: the refusal is the run's.
    assert!(ok.can(&promote_request(None)).await);

    let mut no_staging = over(&fx, &base, &fx.c3).await;
    no_staging.project.deploy.staging = None;
    assert!(!no_staging.can(&r).await);
    let mut no_production = over(&fx, &base, &fx.c3).await;
    no_production.project.deploy.production = target("  ", None);
    assert!(!no_production.can(&r).await);
    let mut straight = over(&fx, &base, &fx.c3).await;
    straight.project.deploy.merges_to = Some("production".into());
    assert!(!straight.can(&r).await);

    let paused = over(&fx, &base, &fx.c3).await;
    paused.paused.set("proj", Some("limit".into()));
    assert!(!paused.can(&r).await);
    paused.paused.set("proj", None);
    paused
        .store_paused
        .set("proj", crate::view::StorePause::Paused("holiday".into()));
    assert!(!paused.can(&r).await);

    // No link to the store: left to a computer that has one.
    let mut unlinked = over(&fx, &base, &fx.c3).await;
    unlinked.access = std::sync::Arc::new(crate::backlog::access::StoredAccess(
        std::sync::Arc::new(super::tests::Keys(None)),
        "https://backlog.example".into(),
    ));
    assert!(!unlinked.can(&r).await);
}

#[tokio::test]
async fn a_request_that_names_no_commit_is_refused_and_so_is_a_project_that_merges_to_production() {
    let fx = Fx::new();
    let (base, seen) = crate::backlog::tests::store(200, json!({ "value": null })).await;
    let l = over(&fx, &base, &fx.c3).await;
    for none in [None, Some("")] {
        assert_eq!(
            l.run(&promote_request(none)).await.unwrap_err(),
            RunError::Refused("Name a commit.".into())
        );
    }
    let mut straight = over(&fx, &base, &fx.c3).await;
    straight.project.deploy.merges_to = Some("production".into());
    // The branches are there and the sha named: it is the project's flow that refuses, first.
    assert_eq!(
        straight.run(&promote_request(Some(&fx.c3))).await.unwrap_err(),
        RunError::Refused("This project's merges go straight to production: there's nothing to promote.".into())
    );
    assert_eq!(fx.production(), fx.c1);
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_promote_pushes_answers_the_sha_and_sends_the_row_the_store_then_holds() {
    let fx = Fx::new();
    let (base, seen) = crate::backlog::tests::store(200, json!({ "value": null })).await;
    let l = over(&fx, &base, &fx.c3).await;
    let out = l.run(&promote_request(Some(&fx.c3[..9]))).await.unwrap();
    assert_eq!(out.status, Finish::Done);
    assert_eq!(
        Value::Object(out.result),
        json!({ "sha": &fx.c3[..9] }),
        "the sha as asked"
    );
    assert_eq!(fx.production(), fx.c3);
    let sent = seen.lock().unwrap().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["op"], "log_add");
    let (kind, entry) = (&sent[0]["args"]["kind"], &sent[0]["args"]["entry"]);
    assert_eq!(kind, "deploy");
    assert_eq!(
        (
            entry["login"].clone(),
            entry["via"].clone(),
            entry["outcome"].clone(),
            entry["environment"].clone()
        ),
        (json!("ann"), json!("site"), json!("pushed"), json!("production"))
    );
    assert_eq!(
        (entry["from"].clone(), entry["to"].clone()),
        (json!(fx.c1), json!(fx.c3))
    );
    assert!(!fx.pending_file().exists(), "gone from disk once the store has it");
}

#[tokio::test]
async fn a_store_that_is_down_keeps_the_row_on_disk_and_the_request_still_ends_done() {
    let fx = Fx::new();
    let l = over(&fx, "http://127.0.0.1:1", &fx.c3).await;
    let out = l.run(&promote_request(Some(&fx.c3))).await.unwrap();
    assert_eq!(out.status, Finish::Done);
    assert_eq!(fx.production(), fx.c3);
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(fx.pending_file()).unwrap()).unwrap();
    assert_eq!(kept.as_array().unwrap().len(), 1);
    assert_eq!(kept[0]["kind"], "deploy");
    assert_eq!(kept[0]["entry"]["outcome"], "pushed");
}

#[tokio::test]
async fn a_refusal_from_the_checks_is_refused_with_the_reason_and_pushes_and_logs_nothing() {
    let fx = Fx::new();
    let (base, seen) = crate::backlog::tests::store(200, json!({ "value": null })).await;
    // Staging serves c2, the request says c3.
    let l = over(&fx, &base, &fx.c2).await;
    let RunError::Refused(why) = l.run(&promote_request(Some(&fx.c3))).await.unwrap_err() else {
        panic!("should refuse")
    };
    assert_eq!(
        why,
        format!("Staging serves {}, not {}: check again.", &fx.c2[..7], &fx.c3[..7])
    );
    assert_eq!(fx.production(), fx.c1);
    assert!(seen.lock().unwrap().is_empty());
    assert!(!fx.pending_file().exists());
}
