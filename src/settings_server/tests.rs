use std::net::Ipv4Addr;
use std::path::Path;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::Request;
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::*;

const HOST: &str = "127.0.0.1:4700";

fn app(settings: Arc<SettingsServer>) -> Router {
    router(settings).layer(MockConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 54321))))
}

fn request(method: &str, path: &str) -> axum::http::request::Builder {
    Request::builder().method(method).uri(path).header("Host", HOST)
}

async fn json_body(res: Response) -> Value {
    let body = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

/// Signs in and `GET /api/settings`, for a test that only cares what comes back.
async fn signed_in_settings(settings: &Arc<SettingsServer>) -> Value {
    let app = app(settings.clone());
    let (cookie, _csrf) = sign_in(&app, settings).await;
    let res = app
        .oneshot(
            request("GET", "/api/settings")
                .header("Cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    json_body(res).await
}

fn mint_token(settings: &SettingsServer, good_for_secs: u64) -> String {
    let token = random_hex(16);
    let expires_at = (settings.now)() + good_for_secs;
    settings.state.lock().unwrap().tokens.insert(token.clone(), expires_at);
    token
}

#[tokio::test]
async fn the_settings_page_has_its_csp_and_a_fresh_nonce_each_time() {
    let (settings, _dir) = test_server();
    let res = app(settings)
        .oneshot(request("GET", "/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let csp = res
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(res.headers().get(header::CACHE_CONTROL).unwrap(), "no-store");
    assert_eq!(res.headers().get(header::X_FRAME_OPTIONS).unwrap(), "DENY");
    let body = String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(!body.contains("{{NONCE}}"));
    let nonce = csp.split("nonce-").nth(1).unwrap().split('\'').next().unwrap();
    assert!(body.contains(nonce));
}

#[tokio::test]
async fn a_wrong_host_is_a_404_like_any_other_path() {
    let (settings, _dir) = test_server();
    let res = app(settings)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/settings")
                .header("Host", "evil.example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert_eq!(json_body(res).await, json!({ "error": "Not found" }));
}

#[tokio::test]
async fn a_host_at_the_wrong_port_is_a_404() {
    let (settings, _dir) = test_server();
    let res = app(settings)
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/settings")
                .header("Host", "127.0.0.1:9999")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_post_with_no_origin_or_the_wrong_one_is_a_404() {
    let (settings, _dir) = test_server();
    let app = app(settings);
    for origin in [None, Some("http://evil.example.com")] {
        let mut builder = request("POST", "/api/settings/restart");
        if let Some(origin) = origin {
            builder = builder.header("Origin", origin);
        }
        let res = app
            .clone()
            .oneshot(builder.body(Body::from("{}")).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn a_post_with_its_own_origin_passes_the_envelope() {
    let (settings, _dir) = test_server();
    let res = app(settings)
        .oneshot(
            request("POST", "/api/settings/session")
                .header("Origin", format!("http://{HOST}"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"token":"nope"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    // Past the envelope, refused for the wrong token instead of 404'd for a bad Origin.
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

fn post_body(path: &str, body: &str) -> Request<Body> {
    request("POST", path)
        .header("Origin", format!("http://{HOST}"))
        .header("Content-Type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn a_missing_wrong_used_or_old_token_is_refused() {
    let (settings, _dir) = test_server();
    let app = app(settings.clone());

    let no_token = app
        .clone()
        .oneshot(post_body("/api/settings/session", "{}"))
        .await
        .unwrap();
    assert_eq!(no_token.status(), StatusCode::UNAUTHORIZED);

    let wrong = app
        .clone()
        .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let old = mint_token(&settings, 0); // already expired
    let old_res = app
        .clone()
        .oneshot(post_body("/api/settings/session", &json!({ "token": old }).to_string()))
        .await
        .unwrap();
    assert_eq!(old_res.status(), StatusCode::UNAUTHORIZED);

    let used = mint_token(&settings, 300);
    let body = json!({ "token": used }).to_string();
    let first = app
        .clone()
        .oneshot(post_body("/api/settings/session", &body))
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let second = app
        .clone()
        .oneshot(post_body("/api/settings/session", &body))
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_good_token_signs_in_with_a_cookie_and_a_csrf_token() {
    let (settings, _dir) = test_server();
    let app = app(settings.clone());
    let token = mint_token(&settings, 300);
    let res = app
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "token": token }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with(&format!("{COOKIE_NAME}=")));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert!(cookie.contains(&format!("Max-Age={SESSION_TTL_SECS}")));
    let body = json_body(res).await;
    assert!(body["csrfToken"].is_string());
}

async fn sign_in(app: &Router, settings: &SettingsServer) -> (String, String) {
    let token = mint_token(settings, 300);
    let res = app
        .clone()
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "token": token }).to_string(),
        ))
        .await
        .unwrap();
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    let cookie = cookie.split(';').next().unwrap().to_string();
    let csrf = json_body(res).await["csrfToken"].as_str().unwrap().to_string();
    (cookie, csrf)
}

#[tokio::test]
async fn settings_without_a_session_are_refused() {
    let (settings, _dir) = test_server();
    let res = app(settings)
        .oneshot(request("GET", "/api/settings").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn signed_in_settings_mirror_the_found_tags_and_say_what_is_set_by_env() {
    let dir = tempfile::tempdir().unwrap();
    let env = |k: &str| (k == "RETASKABLE_TAGS").then(|| "linux".to_string());
    let settings = Arc::new(SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        false,
        env,
        || 1_700_000_000,
        |_| {},
    ));
    let body = signed_in_settings(&settings).await;
    assert_eq!(body["tags"]["effective"], json!(["linux"]));
    assert_eq!(body["envVars"]["RETASKABLE_TAGS"], json!(true));
    assert_eq!(body["envVars"]["RETASKABLE_PORT"], json!(false));
}

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
        false,
        |_: &str| None,
        || 1_700_000_000,
        |_: i32| {},
    ));
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;
    let found = tags::found_tags(&tags::machine_facts(|_: &str| None));
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
async fn restart_calls_exit_with_75_once_signed_in_with_csrf() {
    let dir = tempfile::tempdir().unwrap();
    let exits: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
    let exits_clone = exits.clone();
    let settings = Arc::new(SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        false,
        |_: &str| None,
        || 1_700_000_000,
        move |code: i32| exits_clone.lock().unwrap().push(code),
    ));
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;
    let res = app
        .oneshot(
            request("POST", "/api/settings/restart")
                .header("Origin", format!("http://{HOST}"))
                .header("Cookie", cookie)
                .header("X-CSRF-Token", csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(*exits.lock().unwrap(), vec![75]);
}

#[tokio::test]
async fn five_wrong_tries_wait_a_minute_then_longer_even_for_the_right_one() {
    let now = Arc::new(Mutex::new(1_700_000_000u64));
    let now_clone = now.clone();
    let dir = tempfile::tempdir().unwrap();
    let settings = Arc::new(SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        false,
        |_: &str| None,
        move || *now_clone.lock().unwrap(),
        |_: i32| {},
    ));
    let app = app(settings.clone());

    for _ in 0..5 {
        let res = app
            .clone()
            .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    // Even a right token waits while blocked.
    let good = mint_token(&settings, 300);
    let blocked = app
        .clone()
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "token": good.clone() }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(blocked.headers().get(header::RETRY_AFTER).unwrap(), "60");

    *now.lock().unwrap() += 60;
    let after_first_wait = app
        .clone()
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "token": good }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(after_first_wait.status(), StatusCode::OK);

    // The good sign-in cleared the streak, so a fresh run of 5 wrong tries waits a minute
    // again rather than picking up where the last block left off.
    for _ in 0..5 {
        app.clone()
            .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
            .await
            .unwrap();
    }
    let blocked_again = app
        .clone()
        .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
        .await
        .unwrap();
    assert_eq!(blocked_again.headers().get(header::RETRY_AFTER).unwrap(), "60");
}

#[tokio::test]
async fn a_wait_still_in_force_doubles_the_next_one() {
    let now = Arc::new(Mutex::new(1_700_000_000u64));
    let now_clone = now.clone();
    let (settings, _dir) = {
        let dir = tempfile::tempdir().unwrap();
        let settings = Arc::new(SettingsServer::new(
            dir.path().to_path_buf(),
            4700,
            false,
            |_: &str| None,
            move || *now_clone.lock().unwrap(),
            |_: i32| {},
        ));
        (settings, dir)
    };
    let app = app(settings.clone());

    for _ in 0..5 {
        app.clone()
            .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
            .await
            .unwrap();
    }
    let first_block = app
        .clone()
        .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
        .await
        .unwrap();
    assert_eq!(first_block.headers().get(header::RETRY_AFTER).unwrap(), "60");

    // Waiting out the first block resets the failure count and its window (so a wrong try
    // right after doesn't immediately double the wait again), but keeps the wait length
    // itself: five more wrong tries, still without ever succeeding, double it rather than
    // starting back at a minute.
    *now.lock().unwrap() += 60;
    for _ in 0..5 {
        let res = app
            .clone()
            .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    let second_block = app
        .clone()
        .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
        .await
        .unwrap();
    assert_eq!(second_block.headers().get(header::RETRY_AFTER).unwrap(), "120");
}

#[tokio::test]
async fn a_body_over_16kb_is_refused() {
    let (settings, _dir) = test_server();
    let big = format!(r#"{{"token":"{}"}}"#, "a".repeat(20 * 1024));
    let res = app(settings)
        .oneshot(post_body("/api/settings/session", &big))
        .await
        .unwrap();
    assert!(res.status().is_client_error());
}

fn settings_with_network(dir: &Path, network_enabled: bool, stored: Option<NetworkAccess>) -> Arc<SettingsServer> {
    if let Some(network) = stored {
        MachineStore::load(dir).unwrap().set_network(network).unwrap();
    }
    Arc::new(SettingsServer::new(
        dir.to_path_buf(),
        4700,
        network_enabled,
        |_: &str| None,
        || 1_700_000_000,
        |_: i32| {},
    ))
}

#[tokio::test]
async fn a_good_password_signs_in_and_a_wrong_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(
        dir.path(),
        true,
        Some(NetworkAccess {
            enabled: true,
            password_hash: Some(password::hash_password("a good enough password")),
        }),
    );
    let app = app(settings);
    let wrong = app
        .clone()
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "password": "nope" }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    let right = app
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "password": "a good enough password" }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(right.status(), StatusCode::OK);
    assert!(json_body(right).await["csrfToken"].is_string());
}

#[tokio::test]
async fn a_right_password_is_refused_while_network_access_is_off() {
    // agent-settings.md section 3.1: turning network access off keeps the hash (so turning it
    // back on needs no new password), but it must not go on signing in while it's off.
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(
        dir.path(),
        true,
        Some(NetworkAccess {
            enabled: false,
            password_hash: Some(password::hash_password("a good enough password")),
        }),
    );
    let res = app(settings)
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "password": "a good enough password" }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_password_is_refused_when_none_is_saved_yet() {
    let (settings, _dir) = test_server();
    let res = app(settings)
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "password": "whatever it is" }).to_string(),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn restart_waiting_also_reflects_a_saved_network_access_change() {
    // The agent started with network access off; the file now has it on (saved, not yet
    // restarted into), so a restart is waiting the same way a changed port would say so.
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_with_network(
        dir.path(),
        false,
        Some(NetworkAccess {
            enabled: true,
            password_hash: Some(password::hash_password("a good enough password")),
        }),
    );
    let body = signed_in_settings(&settings).await;
    assert_eq!(body["restartWaiting"], json!(true));
    assert_eq!(body["network"], json!({ "enabled": true, "hasPassword": true }));
}

async fn post_setting(app: &Router, cookie: &str, csrf: &str, body: Value) -> Response {
    app.clone()
        .oneshot(
            request("POST", "/api/settings")
                .header("Origin", format!("http://{HOST}"))
                .header("Cookie", cookie.to_string())
                .header("X-CSRF-Token", csrf.to_string())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
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
    assert_eq!(config::load(dir.path()).unwrap().machine.port, 4800);
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
        false,
        |k: &str| (k == "RETASKABLE_PORT").then(|| "4900".to_string()),
        || 1_700_000_000,
        |_: i32| {},
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
    let settings = settings_with_network(dir.path(), false, None);
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;

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
    let settings = settings_with_network(dir.path(), false, None);
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;

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
