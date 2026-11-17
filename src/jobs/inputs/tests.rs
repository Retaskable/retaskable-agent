use super::*;
use crate::backlog::pool::TaskEnd;
use serde_json::json;

#[test]
fn a_token_is_saved_per_origin_under_a_plain_name() {
    assert_eq!(
        token_name("https://staging.retaskable.com").as_deref(),
        Some("SCAN_TOKEN_STAGING_RETASKABLE_COM")
    );
    assert_eq!(
        token_name("http://127.0.0.1:8080").as_deref(),
        Some("SCAN_TOKEN_127_0_0_1_8080")
    );
    assert_eq!(token_name("not a url"), None);
}

#[test]
fn the_token_is_read_from_the_environment_the_saved_secret_is_put_in_and_a_blank_one_is_none() {
    let origin = "https://staging.example.com";
    let env = Env::from_pairs([("SCAN_TOKEN_STAGING_EXAMPLE_COM", "  t0k  ")]);
    assert_eq!(staging_token(&env, origin).as_deref(), Some("t0k"));
    assert_eq!(
        staging_token(&Env::from_pairs([("SCAN_TOKEN_STAGING_EXAMPLE_COM", " ")]), origin),
        None
    );
    assert_eq!(staging_token(&Env::empty(), origin), None);
    // Another origin never reads it.
    assert_eq!(staging_token(&env, "https://staging.other.com"), None);
}

#[test]
fn the_scan_section_and_deploy_urls_make_the_settings() {
    let cfg: ProjectConfig = serde_json::from_value(json!({
        "repoPath": "/r",
        "scan": { "a11y": true, "pages": ["a"], "signedInPaths": ["/api/me"] },
        "deploy": { "staging": { "url": "https://s.example" } },
    }))
    .unwrap();
    let s = scan_settings(&cfg);
    assert!(s.a11y && !s.probes);
    assert_eq!(s.listed_pages(), ["/a"]);
    assert_eq!(a11y_settings(&cfg).sample_screens.len(), 19);
    let chrome = ChromeFlag::default();
    assert_eq!(
        can_for(&cfg, &chrome, &Env::empty()),
        [Can::Chrome, Can::Repo, Can::Engine]
    );
}

#[test]
fn a_jobs_end_body_becomes_the_task_end_fields() {
    let end = TaskEnd::from_job_body(
        "t",
        &json!({ "ok": true, "result": { "day": "d" }, "known": { "k": "v" } }),
    );
    let wire = serde_json::to_value(&end).unwrap();
    assert_eq!(
        wire,
        json!({ "id": "t", "ok": true, "result": { "day": "d" }, "known": { "k": "v" } })
    );
    let refused = TaskEnd::from_job_body("t", &json!({ "ok": false, "refused": { "why": "no" } }));
    assert_eq!(
        serde_json::to_value(&refused).unwrap(),
        json!({ "id": "t", "ok": false, "refused": { "why": "no" } })
    );
    let failed = TaskEnd::from_job_body(
        "t",
        &json!({ "ok": false, "note": "n", "noRetry": true, "limitHit": { "until": 5 } }),
    );
    let wire = serde_json::to_value(&failed).unwrap();
    assert_eq!(wire["noRetry"], true);
    assert_eq!(wire["limitHit"]["until"], 5);
    // What was kept comes back the same.
    let back: TaskEnd = serde_json::from_value(wire).unwrap();
    assert_eq!(back, failed);
}
