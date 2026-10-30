use axum::body::Body;
use tower::ServiceExt;

use super::super::test_support::*;
use super::super::*;

#[tokio::test]
async fn saving_tags_without_a_csrf_token_is_refused() {
    let (settings, _dir) = test_server();
    let app = app(settings.clone());
    let (cookie, _csrf) = sign_in(&app, &settings).await;
    let res = app
        .oneshot(
            request("POST", "/api/settings")
                .header("Origin", format!("http://{HOST}"))
                .header("Cookie", cookie)
                .body(Body::from(r#"{"tags":["games"]}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn saving_a_typed_tag_list_keeps_only_what_changes_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let settings = Arc::new(SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        4700,
        false,
        Env::empty(),
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(dir.path()),
    ));
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;
    let found = tags::found_tags(&tags::machine_facts(&Env::empty()));
    let mut typed = found.clone();
    typed.push("games".to_string());
    let res = app
        .clone()
        .oneshot(
            request("POST", "/api/settings")
                .header("Origin", format!("http://{HOST}"))
                .header("Cookie", cookie.clone())
                .header("X-CSRF-Token", csrf.clone())
                .body(Body::from(json!({ "tags": typed }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let saved = MachineStore::load(dir.path()).unwrap().tags().unwrap();
    assert_eq!(saved.added, vec!["games".to_string()]);
    assert!(saved.removed.is_empty());

    let get_res = app
        .oneshot(
            request("GET", "/api/settings")
                .header("Cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = json_body(get_res).await;
    let mut effective = body["tags"]["effective"].as_array().unwrap().clone();
    effective.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    let mut expected: Vec<Value> = typed.iter().cloned().map(Value::String).collect();
    expected.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    assert_eq!(effective, expected);
}

#[tokio::test]
async fn an_unsupported_setting_is_refused_by_name() {
    let (settings, _dir) = test_server();
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;
    let res = app
        .oneshot(
            request("POST", "/api/settings")
                .header("Origin", format!("http://{HOST}"))
                .header("Cookie", cookie)
                .header("X-CSRF-Token", csrf)
                .body(Body::from(r#"{"dailyUsd":5}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = json_body(res).await;
    assert!(body["error"].as_str().unwrap().contains("dailyUsd"));
}

#[tokio::test]
async fn saving_the_port_validates_its_range_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(dir.path(), false, None);
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;

    let too_low = post_setting(&app, &cookie, &csrf, json!({ "port": 80 })).await;
    assert_eq!(too_low.status(), StatusCode::BAD_REQUEST);

    let ok = post_setting(&app, &cookie, &csrf, json!({ "port": 4800 })).await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(config::load(dir.path()).unwrap().machine.port, Some(4800));
}

#[tokio::test]
async fn a_bad_field_in_a_multi_field_save_leaves_the_good_ones_unsaved() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(dir.path(), false, None);
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;

    let res = post_setting(&app, &cookie, &csrf, json!({ "tags": ["games"], "port": 80 })).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    assert!(MachineStore::load(dir.path()).unwrap().tags().is_none());
}

#[tokio::test]
async fn saving_the_port_is_refused_when_set_by_env() {
    let dir = tempfile::tempdir().unwrap();
    let settings = Arc::new(SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        4700,
        false,
        Env::from_pairs([("RETASKABLE_PORT", "4900")]),
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(dir.path()),
    ));
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;
    let res = post_setting(&app, &cookie, &csrf, json!({ "port": 4800 })).await;
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = json_body(res).await;
    assert!(body["error"].as_str().unwrap().contains("RETASKABLE_PORT"));
}

#[tokio::test]
async fn saving_how_often_it_checks_validates_its_range_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(dir.path(), false, None);
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;

    let too_high = post_setting(&app, &cookie, &csrf, json!({ "pollIntervalSec": 3601 })).await;
    assert_eq!(too_high.status(), StatusCode::BAD_REQUEST);

    let ok = post_setting(&app, &cookie, &csrf, json!({ "pollIntervalSec": 30 })).await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(config::load(dir.path()).unwrap().machine.poll_interval_sec, 30);
}

#[tokio::test]
async fn saving_the_local_model_validates_its_name_and_address_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(dir.path(), false, None);
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;

    let bad_name = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({ "local": { "enabled": true, "model": "model name", "baseUrl": "http://localhost:11436" } }),
    )
    .await;
    assert_eq!(bad_name.status(), StatusCode::BAD_REQUEST);

    let bad_url = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({ "local": { "enabled": false, "model": "", "baseUrl": "localhost:11436" } }),
    )
    .await;
    assert_eq!(bad_url.status(), StatusCode::BAD_REQUEST);

    let ok = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({ "local": { "enabled": true, "model": "llama3.1", "baseUrl": "http://10.0.0.5:11436" } }),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    let saved = config::load(dir.path()).unwrap().machine.local;
    assert!(saved.enabled);
    assert_eq!(saved.model, "llama3.1");
    assert_eq!(saved.base_url, "http://10.0.0.5:11436");
}

#[tokio::test]
async fn saving_the_engine_default_validates_it_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(dir.path(), false, None);
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;

    let bad = post_setting(&app, &cookie, &csrf, json!({ "engineChoice": { "default": "steam" } })).await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    let ok = post_setting(&app, &cookie, &csrf, json!({ "engineChoice": { "default": "local" } })).await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(
        config::load(dir.path()).unwrap().machine.engine_choice.default,
        Some(config::Engine::Local)
    );
}

#[tokio::test]
async fn saving_the_engines_role_overrides_validates_them_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let (_settings, app, cookie, csrf) = signed_in_app(dir.path()).await;

    let bad = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({ "engineChoice": { "default": "claude", "roles": { "implementer": "steam" } } }),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    let ok = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({
            "engineChoice": {
                "default": "claude",
                "roles": { "implementer": "cursor", "review": "default", "chat": "local" },
            },
        }),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    let saved = config::load(dir.path()).unwrap().machine.engine_choice.roles;
    assert_eq!(saved.implementer, Some(config::Engine::Cursor));
    assert_eq!(saved.review, None);
    assert_eq!(saved.chat, Some(config::Engine::Local));
    assert_eq!(saved.planner, None);
}

#[tokio::test]
async fn saving_the_engines_lane_overrides_validates_them_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let (settings, app, cookie, csrf) = signed_in_app(dir.path()).await;

    let bad = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({ "engineChoice": { "default": "claude", "lanes": ["claude", "steam"] } }),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    let ok = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({ "engineChoice": { "default": "claude", "lanes": ["claude", "cursor", "default"] } }),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(
        config::load(dir.path()).unwrap().machine.engine_choice.lanes,
        vec![config::LaneEngine::Claude, config::LaneEngine::Cursor]
    );

    let settings_value = signed_in_settings(&settings).await;
    assert_eq!(settings_value["engineChoice"]["lanes"], json!(["claude", "cursor"]));

    // A lane can be set to Codex, and the refusal names it among the picks.
    let codex = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({ "engineChoice": { "default": "codex", "roles": { "review": "codex" }, "lanes": ["codex", "default"] } }),
    )
    .await;
    assert_eq!(codex.status(), StatusCode::OK);
    let saved = config::load(dir.path()).unwrap().machine.engine_choice;
    assert_eq!(saved.default, Some(config::Engine::Codex));
    assert_eq!(saved.roles.review, Some(config::Engine::Codex));
    assert_eq!(saved.lanes, vec![config::LaneEngine::Codex]);
    let settings_value = signed_in_settings(&settings).await;
    assert_eq!(settings_value["engineChoice"]["default"], json!("codex"));
    assert_eq!(settings_value["engineChoice"]["lanes"], json!(["codex"]));
}

#[tokio::test]
async fn saving_cursor_models_validates_names_and_an_empty_one_is_auto() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(dir.path(), false, None);
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;

    let bad = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({ "cursorModels": { "top": "--force", "implementer": null, "scout": null } }),
    )
    .await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    let ok = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({ "cursorModels": { "top": "gpt-5", "implementer": "", "scout": null } }),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    let saved = config::load(dir.path()).unwrap().machine.cursor.models;
    assert_eq!(saved.top, Some("gpt-5".to_string()));
    assert_eq!(saved.implementer, None);
    assert_eq!(saved.scout, None);
}

#[tokio::test]
async fn saving_network_access_needs_a_long_enough_password_and_turning_it_off_keeps_the_hash() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(dir.path(), false, None);
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;

    // On with no password at all, and nothing saved yet to fall back to.
    let no_password = post_setting(&app, &cookie, &csrf, json!({ "network": { "enabled": true } })).await;
    assert_eq!(no_password.status(), StatusCode::BAD_REQUEST);

    // On with a password that's too short.
    let short = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({ "network": { "enabled": true, "password": "short" } }),
    )
    .await;
    assert_eq!(short.status(), StatusCode::BAD_REQUEST);

    let ok = post_setting(
        &app,
        &cookie,
        &csrf,
        json!({ "network": { "enabled": true, "password": "a good enough password" } }),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    let saved = MachineStore::load(dir.path()).unwrap().network().unwrap();
    assert!(saved.enabled);
    let hash = saved.password_hash.clone().unwrap();

    // Turning it off with no new password keeps the hash, so turning it on again needs none.
    let off = post_setting(&app, &cookie, &csrf, json!({ "network": { "enabled": false } })).await;
    assert_eq!(off.status(), StatusCode::OK);
    let saved_off = MachineStore::load(dir.path()).unwrap().network().unwrap();
    assert!(!saved_off.enabled);
    assert_eq!(saved_off.password_hash, Some(hash));
}
