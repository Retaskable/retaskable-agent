use axum::body::Body;
use axum::http::Request;
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::test_support::*;
use super::*;
use crate::password;

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
    let env = Env::from_pairs([("RETASKABLE_TAGS", "linux")]);
    let settings = Arc::new(SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        4700,
        false,
        env,
        || 1_700_000_000,
        |_| {},
        test_keychain(dir.path()),
    ));
    let body = signed_in_settings(&settings).await;
    assert_eq!(body["tags"]["effective"], json!(["linux"]));
    assert_eq!(body["envVars"]["RETASKABLE_TAGS"], json!(true));
    assert_eq!(body["envVars"]["RETASKABLE_PORT"], json!(false));
}

#[tokio::test]
async fn restart_calls_exit_with_75_once_signed_in_with_csrf() {
    let dir = tempfile::tempdir().unwrap();
    let exits: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
    let exits_clone = exits.clone();
    let settings = Arc::new(SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        4700,
        false,
        Env::empty(),
        || 1_700_000_000,
        move |code: i32| exits_clone.lock().unwrap().push(code),
        test_keychain(dir.path()),
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
        4700,
        false,
        Env::empty(),
        move || *now_clone.lock().unwrap(),
        |_: i32| {},
        test_keychain(dir.path()),
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
            4700,
            false,
            Env::empty(),
            move || *now_clone.lock().unwrap(),
            |_: i32| {},
            test_keychain(dir.path()),
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

#[tokio::test]
async fn link_without_a_session_is_refused() {
    let (settings, _dir) = test_server();
    let res = app(settings)
        .oneshot(request("GET", "/api/link").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn link_lists_the_three_environments_and_which_one_is_connected() {
    let dir = tempfile::tempdir().unwrap();
    let keychain = test_keychain(dir.path());
    keychain.set("https://staging.retaskable.com", "refresh-token").unwrap();
    let settings = test_server_with_keychain(dir.path(), keychain);
    let app = app(settings.clone());
    let (cookie, _csrf) = sign_in(&app, &settings).await;
    let res = app
        .oneshot(
            request("GET", "/api/link")
                .header("Cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    let environments = body["environments"].as_array().unwrap();
    assert_eq!(environments.len(), 3);
    let staging = environments
        .iter()
        .find(|e| e["name"].as_str() == Some("Staging"))
        .unwrap();
    assert_eq!(staging["connected"], json!(true));
    let production = environments
        .iter()
        .find(|e| e["name"].as_str() == Some("Production"))
        .unwrap();
    assert_eq!(production["connected"], json!(false));
}

#[tokio::test]
async fn profile_without_a_session_is_refused() {
    let (settings, _dir) = test_server();
    let res = app(settings)
        .oneshot(request("GET", "/api/profile").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
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

#[tokio::test]
async fn the_status_route_needs_a_sign_in() {
    let (settings, _dir) = test_server();
    let res = app(settings)
        .oneshot(request("GET", "/api/settings/status").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_status_route_answers_the_now_tab_once_the_agent_gave_it_a_source() {
    let dir = tempfile::tempdir().unwrap();
    let (settings, app, cookie, _csrf) = signed_in_app(dir.path()).await;
    let get = |app: Router, cookie: String| async move {
        app.oneshot(
            request("GET", "/api/settings/status")
                .header("Cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    };
    // Before the agent has handed over its view there is nothing true to say.
    assert_eq!(
        get(app.clone(), cookie.clone()).await.status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    settings.set_status_source(StatusSource {
        view: crate::view::AgentView {
            machine: "pc".into(),
            tags: vec![],
            projects: vec![],
            version: "1.2.3".into(),
            lanes: 3,
            chore_lane: None,
            lane_sizes: Default::default(),
            paused: Default::default(),
        },
        started: std::time::Instant::now(),
        checks: Arc::new(Mutex::new(None)),
        live: Default::default(),
    });
    let res = get(app, cookie).await;
    assert_eq!(res.status(), StatusCode::OK);
    let body = json_body(res).await;
    assert_eq!(body["header"]["name"], "pc");
    assert_eq!(body["header"]["version"], "1.2.3");
    assert_eq!(
        body["lanes"].as_array().unwrap().len(),
        2,
        "the config's lane count, re-read now"
    );
    assert_eq!(body["engines"].as_array().unwrap().len(), 5);
}
