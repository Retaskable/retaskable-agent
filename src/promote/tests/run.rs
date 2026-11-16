use serde_json::json;

use super::fixture::{Fx, commit, git};
use crate::promote::git::{push_args, run as git_run};
use crate::promote::run::Pushing;
use crate::promote::{PUSH_TIMEOUT, Stop, Via};

fn refused(why: &str) -> Result<String, Stop> {
    Err(Stop::Refused(why.into()))
}

/// The rows waiting for the store, as written to disk.
fn rows(fx: &Fx) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(fx.pending_file()).unwrap_or_else(|_| "[]".into())).unwrap()
}

#[tokio::test]
async fn pushes_exactly_the_commit_named_as_a_fast_forward_and_logs_it() {
    let fx = Fx::new();
    let p = fx.promoter(fx.gh("machine-login", Some((true, true)), &[]), None);
    // Not staging's head: the commit shown, however far staging has moved.
    assert_eq!(p.promote(&fx.c2[..9], Via::Auto).await, Ok(fx.c2.clone()));
    assert_eq!(fx.production(), fx.c2);
    // Origin's own reflog: production moved once, from c1 to c2, and c2 descends from c1 (a fast-forward).
    assert_eq!(fx.reflog(), [fx.c2.clone(), fx.c1.clone()]);
    git(&fx.origin, &["merge-base", "--is-ancestor", &fx.c1, &fx.c2]);
    // The refetch: this checkout sees where production is now.
    assert_eq!(git(&fx.work, &["rev-parse", "origin/production"]), fx.c2);
    // The row, on disk, nothing sent (this promoter has no store).
    assert_eq!(
        rows(&fx),
        json!([{ "kind": "deploy", "at": rows(&fx)[0]["at"], "entry": {
            "at": "2023-11-14T22:13:20.000Z", "login": "machine-login", "environment": "production",
            "from": fx.c1, "to": fx.c2, "via": "auto", "outcome": "pushed",
        }}])
    );
    // gh asked who this computer is and whether it is an admin, nothing else.
    assert_eq!(
        fx.gh_calls(),
        ["api repos/o/r --jq .permissions", "api user --jq .login"]
    );
}

#[tokio::test]
async fn the_websites_promote_names_who_pressed_it_and_the_machine_has_no_via() {
    let fx = Fx::new();
    let p = fx.promoter(fx.gh("machine-login", Some((true, true)), &[]), None);
    assert_eq!(p.promote(&fx.c2, Via::Site("ann".into())).await, Ok(fx.c2.clone()));
    assert_eq!(p.promote(&fx.c3, Via::Machine).await, Ok(fx.c3.clone()));
    let rows = rows(&fx);
    let entry = |i: usize| rows[i]["entry"].clone();
    assert_eq!(
        (entry(0)["login"].clone(), entry(0)["via"].clone()),
        (json!("ann"), json!("site"))
    );
    assert_eq!(entry(1)["login"], "machine-login");
    assert!(entry(1).get("via").is_none());
    assert_eq!(entry(1)["from"], fx.c2.as_str());
    // The website's login is never asked of gh.
    assert_eq!(
        fx.gh_calls()
            .iter()
            .filter(|c| c.as_str() == "api user --jq .login")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_push_that_is_not_a_fast_forward_is_git_s_to_refuse_and_production_stays() {
    let fx = Fx::new();
    // A commit that does not descend from production's.
    git(&fx.work, &["checkout", "-q", "--orphan", "other"]);
    let stray = commit(&fx.work, "z.txt");
    let args = push_args(&stray, "production");
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git_run(&fx.work, &args, PUSH_TIMEOUT).await.unwrap();
    assert!(!out.ok, "{}", out.stderr);
    assert!(out.stderr.contains("rejected"), "{}", out.stderr);
    assert_eq!(fx.production(), fx.c1);
    assert_eq!(fx.reflog(), std::slice::from_ref(&fx.c1));
}

#[tokio::test]
async fn a_commit_that_is_not_on_staging_is_refused_and_nothing_is_pushed() {
    let fx = Fx::new();
    git(&fx.work, &["checkout", "-q", "main"]);
    let off = commit(&fx.work, "m.txt");
    git(&fx.work, &["push", "-q", "origin", "main"]);
    let p = fx.promoter(fx.gh("m", Some((true, true)), &[]), None);
    assert_eq!(
        p.promote(&off, Via::Auto).await,
        refused(&format!("{} isn't on staging", &off[..7]))
    );
    assert_eq!(fx.production(), fx.c1);
    assert!(!fx.pending_file().exists(), "a refusal before the push is no row");
    assert!(fx.gh_calls().is_empty(), "the git checks come before gh is asked");
}

#[tokio::test]
async fn what_production_has_already_is_up_to_date() {
    let fx = Fx::new();
    let p = fx.promoter(fx.gh("m", Some((true, true)), &[]), None);
    assert_eq!(p.promote(&fx.c1, Via::Auto).await, refused("Up to date"));
    assert_eq!(p.promote(&fx.c3, Via::Auto).await, Ok(fx.c3.clone()));
    assert_eq!(p.promote(&fx.c2, Via::Auto).await, refused("Up to date"));
    assert_eq!(fx.reflog(), [fx.c3.clone(), fx.c1.clone()]);
}

#[tokio::test]
async fn production_ahead_of_staging_is_fixed_by_hand_not_by_a_push() {
    let fx = Fx::new();
    git(&fx.work, &["checkout", "-q", "-b", "hotfix", "main"]);
    let hot = commit(&fx.work, "hot.txt");
    git(&fx.work, &["push", "-q", "origin", "hotfix:production"]);
    let p = fx.promoter(fx.gh("m", Some((true, true)), &[]), None);
    assert_eq!(
        p.promote(&fx.c3, Via::Auto).await,
        refused("Production has commits staging doesn't: fix that by hand")
    );
    assert_eq!(fx.production(), hot);
}

#[tokio::test]
async fn only_the_machines_own_admin_login_pushes() {
    let fx = Fx::new();
    let write = fx.promoter(fx.gh("m", Some((false, true)), &[]), None);
    assert_eq!(
        write.promote(&fx.c3, Via::Auto).await,
        refused("Only the repo's admins can promote (you have write access)")
    );
    let read = fx.promoter(fx.gh("m", Some((false, false)), &[]), None);
    assert_eq!(
        read.promote(&fx.c3, Via::Auto).await,
        refused("Only the repo's admins can promote")
    );
    let down = fx.promoter(fx.gh("m", None, &[]), None);
    assert_eq!(
        down.promote(&fx.c3, Via::Auto).await,
        refused("Couldn't ask GitHub whether you're an admin of o/r: HTTP 403: nope")
    );
    assert_eq!(fx.production(), fx.c1);
    assert!(!fx.pending_file().exists());
}

#[tokio::test]
async fn a_bad_commit_name_or_one_this_repo_lacks_is_refused() {
    let fx = Fx::new();
    let p = fx.promoter(fx.gh("m", Some((true, true)), &[]), None);
    for bad in ["", "abc", "main", "012345g", &"a".repeat(41)] {
        assert_eq!(
            p.promote(bad, Via::Auto).await,
            refused("Promote needs the commit to push."),
            "{bad:?}"
        );
    }
    assert_eq!(
        p.promote("1234567", Via::Auto).await,
        refused("1234567 isn't a commit in this repo")
    );
}

#[tokio::test]
async fn a_project_without_both_branches_has_nothing_to_promote_from() {
    let fx = Fx::new();
    let mut p = fx.promoter(fx.gh("m", Some((true, true)), &[]), None);
    p.deploy.production = None;
    let Err(Stop::Refused(why)) = p.promote(&fx.c3, Via::Auto).await else {
        panic!("should refuse")
    };
    assert!(
        why.starts_with("This project has no staging and production branches"),
        "{why}"
    );
}

#[tokio::test]
async fn one_promote_per_repo_at_a_time() {
    let fx = Fx::new();
    let p = fx.promoter(fx.gh("m", Some((true, true)), &[]), None);
    let held = Pushing::take(&fx.work).expect("free");
    assert!(Pushing::take(&fx.work).is_none(), "a second place is not given");
    assert_eq!(
        p.promote(&fx.c3, Via::Auto).await,
        refused("A promote is already being pushed.")
    );
    assert_eq!(fx.production(), fx.c1, "refused, not queued");
    drop(held);
    assert_eq!(p.promote(&fx.c3, Via::Auto).await, Ok(fx.c3.clone()));
    // Another repo is not held up by it.
    let other = Fx::new();
    let _held = Pushing::take(&fx.work).expect("released after the push");
    assert!(Pushing::take(&other.work).is_some());
}

#[tokio::test]
async fn two_at_once_push_once_and_the_loser_is_told() {
    let fx = Fx::new();
    let p = fx.promoter(fx.gh("m", Some((true, true)), &[]), None);
    let (a, b) = tokio::join!(p.promote(&fx.c3, Via::Auto), p.promote(&fx.c3, Via::Auto));
    let mut got = [a, b];
    got.sort_by_key(|r| r.is_err());
    assert_eq!(got[0], Ok(fx.c3.clone()));
    // Whichever came second was held off or found production already there.
    assert!(
        matches!(&got[1], Err(Stop::Refused(w)) if w == "A promote is already being pushed." || w == "Up to date"),
        "{:?}",
        got[1]
    );
    assert_eq!(fx.reflog(), [fx.c3.clone(), fx.c1.clone()], "pushed once");
}

#[tokio::test]
async fn a_push_github_refuses_is_logged_refused_with_the_remotes_words_and_leaves_production() {
    let fx = Fx::new();
    let hook = fx.origin.join("hooks/pre-receive");
    crate::test_support::write_executable(&hook, "#!/bin/sh\necho 'locked for the release freeze' >&2\nexit 1\n");
    let p = fx.promoter(fx.gh("machine-login", Some((true, true)), &[]), None);
    let Err(Stop::Refused(why)) = p.promote(&fx.c3, Via::Site("ann".into())).await else {
        panic!("should refuse")
    };
    assert!(why.starts_with("GitHub refused the push: "), "{why}");
    assert!(why.contains("locked for the release freeze"), "{why}");
    assert!(!why.contains("remote:") && !why.contains("To "), "{why}");
    assert_eq!(fx.production(), fx.c1);
    let rows = rows(&fx);
    let entry = &rows[0]["entry"];
    assert_eq!(rows.as_array().unwrap().len(), 1);
    assert_eq!(
        (entry["outcome"].clone(), entry["via"].clone(), entry["login"].clone()),
        (json!("refused"), json!("site"), json!("ann"))
    );
    assert_eq!(
        (entry["from"].clone(), entry["to"].clone()),
        (json!(fx.c1), json!(fx.c3))
    );
    assert_eq!(entry["at"], json!("2023-11-14T22:13:20.000Z"));
    assert_eq!(entry["reason"], why.trim_start_matches("GitHub refused the push: "));
}
