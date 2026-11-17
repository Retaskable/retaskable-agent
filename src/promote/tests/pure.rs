use crate::config::{DeployConfig, DeployTarget};
use crate::promote::git::push_args;
use crate::promote::site::{is_login, version_url};
use crate::promote::{environments_of, merges_to_production, push_refusal, short};

fn target(branch: &str, url: Option<&str>) -> Option<DeployTarget> {
    Some(DeployTarget {
        branch: branch.into(),
        url: url.map(String::from),
        smoke: None,
        version_path: None,
        rollout: None,
    })
}

fn deploy(staging: &str, production: &str, merges_to: Option<&str>) -> DeployConfig {
    DeployConfig {
        merges_to: merges_to.map(String::from),
        staging: target(staging, Some(" https://stg.example/ ")),
        production: target(production, Some("")),
    }
}

#[test]
fn environments_need_both_branches_and_merges_that_go_to_staging_first() {
    let env = environments_of(&deploy(" staging ", "main", None)).unwrap();
    assert_eq!(
        (env.staging.branch.as_str(), env.production.branch.as_str()),
        ("staging", "main")
    );
    assert_eq!(env.staging.url.as_deref(), Some("https://stg.example/"), "trimmed");
    assert_eq!(env.production.url, None, "an empty address is none");
    assert!(environments_of(&deploy("staging", "  ", None)).is_none());
    assert!(environments_of(&deploy("", "main", None)).is_none());
    assert!(environments_of(&DeployConfig::default()).is_none());
    // Any other `mergesTo` is staging, as the TypeScript's.
    assert!(environments_of(&deploy("staging", "main", Some("staging"))).is_some());
    assert!(environments_of(&deploy("staging", "main", Some("whatever"))).is_some());
    let straight = deploy("staging", "main", Some("production"));
    assert!(merges_to_production(&straight));
    assert!(environments_of(&straight).is_none());
}

#[test]
fn the_push_is_one_plain_refspec_and_never_a_force() {
    let full = "0123456789abcdef0123456789abcdef01234567";
    assert_eq!(
        push_args(full, "production"),
        [
            "push",
            "origin",
            "0123456789abcdef0123456789abcdef01234567:refs/heads/production"
        ]
    );
    for branch in ["main", "release/1", "+main", "--force"] {
        let args = push_args(full, branch);
        assert_eq!(args.len(), 3);
        assert_eq!(
            args[2],
            format!("{full}:refs/heads/{branch}"),
            "one argument, branch and all"
        );
        assert!(args[2].starts_with(full), "so no leading + on the refspec");
        assert_eq!(args[..2], ["push", "origin"], "no flag before the refspec");
    }
}

#[test]
fn a_refused_pushs_words_lose_git_s_hints_and_the_remote_prefix() {
    let out = "remote: error: GH006: Protected branch update failed for refs/heads/main.\n\
               remote: \n\
               To github.com:o/r.git\n\
               hint: Updates were rejected because the tip is behind\n\
               Everything up-to-date\n\
               \x20! [remote rejected] abc -> main (protected branch hook declined)\n\
               error: failed to push some refs to 'github.com:o/r.git'\r\n";
    assert_eq!(
        push_refusal(out),
        "error: GH006: Protected branch update failed for refs/heads/main.\n! [remote rejected] abc -> main (protected branch hook declined)\nerror: failed to push some refs to 'github.com:o/r.git'"
    );
    assert_eq!(push_refusal(""), "the push failed");
    assert_eq!(push_refusal("To x\nhint: y\n\nremote:\n"), "the push failed");
    assert_eq!(push_refusal(&"x".repeat(900)).chars().count(), 500);
}

#[test]
fn a_version_address_is_the_url_and_its_path() {
    assert_eq!(version_url("https://a.test", None), "https://a.test/version.json");
    assert_eq!(
        version_url("https://a.test///", Some(" v.json ")),
        "https://a.test/v.json"
    );
    assert_eq!(
        version_url("https://a.test/app/", Some("/sha")),
        "https://a.test/app/sha"
    );
    assert_eq!(version_url("https://a.test", Some("  ")), "https://a.test/version.json");
}

#[test]
fn only_a_login_shaped_name_reaches_github() {
    for ok in ["ann", "A", "a-b", "0x", &"a".repeat(39)] {
        assert!(is_login(ok), "{ok}");
    }
    for bad in ["", "-ann", "a_b", "a/b", "a b", "ann\n", "../x", &"a".repeat(40)] {
        assert!(!is_login(bad), "{bad:?}");
    }
    assert_eq!(short("0123456789"), "0123456");
}
