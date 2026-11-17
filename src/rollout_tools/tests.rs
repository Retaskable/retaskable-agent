use super::*;
use serde_json::json;

fn deploy(v: serde_json::Value) -> DeployConfig {
    serde_json::from_value(v).unwrap()
}

fn rollout(traffic: &str, health: &str) -> DeployConfig {
    deploy(json!({
        "staging": { "branch": "staging" },
        "production": { "branch": "main", "url": "https://x.test",
                        "rollout": { "traffic": traffic, "health": health } }
    }))
}

fn everything(_: &str) -> bool {
    true
}

#[test]
fn a_rollout_needs_both_commands_a_production_branch_and_address_and_a_staging_branch() {
    assert_eq!(
        rollout_of(&rollout(" t ", "h")),
        Some(("t".to_string(), "h".to_string()))
    );
    assert_eq!(rollout_of(&rollout("t", " ")), None);
    assert_eq!(rollout_of(&DeployConfig::default()), None);
    let no_address = deploy(json!({ "staging": { "branch": "s" },
        "production": { "branch": "main", "rollout": { "traffic": "t", "health": "h" } } }));
    assert_eq!(rollout_of(&no_address), None);
    let no_staging = deploy(json!({ "production": { "branch": "main", "url": "https://x.test",
        "rollout": { "traffic": "t", "health": "h" } } }));
    assert_eq!(rollout_of(&no_staging), None);
    let straight = deploy(json!({ "mergesTo": "production", "production": { "branch": "main",
        "url": "https://x.test", "rollout": { "traffic": "t", "health": "h" } } }));
    assert!(rollout_of(&straight).is_some());
}

#[test]
fn no_rollout_means_nothing_is_missing() {
    assert_eq!(missing(&DeployConfig::default(), &Env::empty(), &|_| false), None);
}

#[test]
fn a_program_that_isnt_installed_is_named() {
    let d = rollout("deploytool traffic", "curl health");
    let has = |p: &str| p == "curl";
    assert_eq!(
        missing(&d, &Env::empty(), &has).as_deref(),
        Some("deploytool isn't installed on this computer, so it can't run the rollout")
    );
}

#[test]
fn a_cloudflare_rollout_needs_the_account_id_and_a_real_api_token() {
    let d = rollout("npm run rollout:traffic", "npm run rollout:health");
    let say = |env: &Env| missing(&d, env, &everything);
    assert!(say(&Env::empty()).unwrap().starts_with("CLOUDFLARE_ACCOUNT_ID"));
    let id = [("CLOUDFLARE_ACCOUNT_ID", "acc")];
    assert!(say(&Env::from_pairs(id)).unwrap().starts_with("CLOUDFLARE_API_TOKEN"));
    let sessions = Env::from_pairs([
        ("CLOUDFLARE_ACCOUNT_ID", "acc"),
        ("CLOUDFLARE_API_TOKEN", NO_DEPLOY_TOKEN),
    ]);
    assert!(say(&sessions).unwrap().starts_with("CLOUDFLARE_API_TOKEN"));
    let both = Env::from_pairs([("CLOUDFLARE_ACCOUNT_ID", "acc"), ("CLOUDFLARE_API_TOKEN", "tok")]);
    assert_eq!(say(&both), None);
}

#[test]
fn a_rollout_that_doesnt_reach_cloudflare_needs_neither() {
    let d = rollout("./traffic.sh", "./health.sh");
    assert_eq!(missing(&d, &Env::empty(), &everything), None);
    // Only a whole word counts: `mywrangler` and `rollout:trafficx` are not it.
    let near = rollout("mywrangler", "rollout:trafficx");
    assert_eq!(missing(&near, &Env::empty(), &everything), None);
    let wrangler = rollout("npx wrangler versions", "./health.sh");
    assert!(missing(&wrangler, &Env::empty(), &everything).is_some());
}

#[test]
fn on_path_finds_a_program_in_the_path_and_a_path_that_exists() {
    let dir = tempfile::tempdir().unwrap();
    let name = if cfg!(windows) { "tool.exe" } else { "tool" };
    std::fs::write(dir.path().join(name), "").unwrap();
    let env = Env::from_pairs([("PATH", dir.path().to_str().unwrap())]);
    assert!(on_path("tool", &env));
    assert!(!on_path("other", &env));
    assert!(on_path(dir.path().join(name).to_str().unwrap(), &Env::empty()));
    assert!(!on_path(dir.path().join("nope").to_str().unwrap(), &Env::empty()));
}
