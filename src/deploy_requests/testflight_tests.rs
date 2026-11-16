//! The `testflight_build` request against a fake runner: who claims it, and how a build's end becomes the request's.
use serde_json::{Value, json};

use super::testflight::{can, run};
use super::tests::{live, request};
use super::{Deps, RunError};
use crate::backlog::deploy::{DeployKind, DeployRequest, Finish};
use crate::promote::DeployLog;
use crate::testflight::fake::{BUILD_NUMBER, Fake, Project, SHA, exited};

fn asked(sha: Option<&str>, by: &str) -> DeployRequest {
    let mut r = request("dr-1", DeployKind::TestflightBuild);
    r.args.sha = sha.map(String::from);
    r.issued_by = by.into();
    r
}

fn log_of(p: &Project) -> DeployLog {
    DeployLog::open(&p.dir().join(crate::promote::log::FILE))
}

#[tokio::test]
async fn a_computer_that_is_not_a_mac_never_claims_it() {
    let p = Project::new();
    let mut f = Fake::new();
    assert!(can(&p.config, &f, &asked(Some(SHA), "ann")).await);
    f.mac = false;
    assert!(!can(&p.config, &f, &asked(Some(SHA), "ann")).await);
    // The real thing says the same on any computer that isn't a Mac, whatever else is set (on a Mac it would ask `gh`).
    #[cfg(not(target_os = "macos"))]
    {
        let mut real = live("http://127.0.0.1:1", Some("tok"));
        real.project = p.config.clone();
        assert!(!real.can(&asked(Some(SHA), "ann")).await);
        let mut any = real;
        any.project.release.testflight_any_member = true;
        assert!(!any.can(&asked(Some(SHA), "ann")).await);
    }
}

#[tokio::test]
async fn it_needs_the_setting_the_project_and_xcodes_files() {
    let mut p = Project::new();
    let f = Fake::new();
    p.config.release.testflight = false;
    assert!(!can(&p.config, &f, &asked(Some(SHA), "ann")).await);
    p.config.release.testflight = true;
    std::fs::remove_file(
        std::path::Path::new(&p.config.repo_path)
            .join("app")
            .join(crate::testflight::build_script()),
    )
    .unwrap();
    assert!(!can(&p.config, &f, &asked(Some(SHA), "ann")).await);
}

#[tokio::test]
async fn only_the_owner_claims_it_unless_any_member_is_on_and_logins_ignore_case() {
    let mut p = Project::new();
    let mut f = Fake::new();
    f.login = Ok("Ann".into());
    assert!(can(&p.config, &f, &asked(Some(SHA), "ANN")).await);
    assert!(!can(&p.config, &f, &asked(Some(SHA), "bob")).await);
    p.config.release.testflight_any_member = true;
    assert!(can(&p.config, &f, &asked(Some(SHA), "bob")).await);
    // A computer that can't say who it is leaves it to the build's own admin check.
    p.config.release.testflight_any_member = false;
    f.login = Err("gh is not signed in".into());
    assert!(can(&p.config, &f, &asked(Some(SHA), "bob")).await);
    f.login = Ok(String::new());
    assert!(can(&p.config, &f, &asked(Some(SHA), "bob")).await);
}

#[tokio::test]
async fn a_request_that_names_no_commit_is_refused_before_anything_else() {
    let mut p = Project::new();
    p.config.release.testflight = false;
    let f = Fake::new();
    for none in [None, Some("")] {
        assert_eq!(
            run(&p.config, &f, &log_of(&p), &p.dir(), &asked(none, "ann"))
                .await
                .unwrap_err(),
            RunError::Refused("Name a commit.".into())
        );
    }
    assert!(f.lines().is_empty());
}

#[tokio::test]
async fn a_refusal_is_refused_with_its_words_and_runs_nothing() {
    let mut p = Project::new();
    p.config.release.testflight = false;
    let f = Fake::new();
    assert_eq!(
        run(&p.config, &f, &log_of(&p), &p.dir(), &asked(Some(SHA), "ann"))
            .await
            .unwrap_err(),
        RunError::Refused("TestFlight builds are off on this computer: turn them on in its settings.".into())
    );
    assert!(f.lines().is_empty() && !p.dir().exists());
}

#[tokio::test]
async fn done_only_when_submitted_and_else_failed_with_the_same_result() {
    for (end, status, result) in [
        (None, Finish::Done, "submitted"),
        (Some(exited(1, "boom")), Finish::Failed, "failed"),
        (
            Some(crate::testflight::Ran {
                cut: true,
                ..Default::default()
            }),
            Finish::Failed,
            "cancelled",
        ),
    ] {
        let p = Project::new();
        let f = Fake::new();
        if let Some(end) = end {
            f.with(|s| s.ends.insert("bash".into(), end));
        }
        let out = run(&p.config, &f, &log_of(&p), &p.dir(), &asked(Some(&SHA[..9]), "ann"))
            .await
            .unwrap();
        assert_eq!(out.status, status, "{result}");
        // The full commit the environment serves, not the abbreviation asked for; no error text goes to the store.
        assert_eq!(
            Value::Object(out.result),
            json!({ "sha": SHA, "buildNumber": BUILD_NUMBER, "result": result })
        );
    }
}

#[tokio::test]
async fn staging_is_built_only_when_the_request_says_so() {
    let p = Project::new();
    let f = Fake::new();
    let mut r = asked(Some(SHA), "ann");
    r.args.environment = Some("staging".into());
    run(&p.config, &f, &log_of(&p), &p.dir(), &r).await.unwrap();
    assert!(
        f.step("bash")
            .unwrap()
            .env
            .iter()
            .any(|(k, _)| k == "RETASKABLE_CLOUD_ORIGIN")
    );
}
