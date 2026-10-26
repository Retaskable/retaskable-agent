use std::net::Ipv4Addr;

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
        env,
        || 1_700_000_000,
        |_| {},
    ));
    let app = app(settings.clone());
    let (cookie, _csrf) = sign_in(&app, &settings).await;
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
    let body = json_body(res).await;
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
                .body(Body::from(r#"{"port":4800}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    let body = json_body(res).await;
    assert!(body["error"].as_str().unwrap().contains("port"));
}

#[tokio::test]
async fn restart_calls_exit_with_75_once_signed_in_with_csrf() {
    let dir = tempfile::tempdir().unwrap();
    let exits: Arc<Mutex<Vec<i32>>> = Arc::new(Mutex::new(Vec::new()));
    let exits_clone = exits.clone();
    let settings = Arc::new(SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
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

    // Waiting out the first block and failing again, still without ever succeeding,
    // doubles the wait rather than starting back at a minute.
    *now.lock().unwrap() += 60;
    let sixth_failure = app
        .clone()
        .oneshot(post_body("/api/settings/session", r#"{"token":"nope"}"#))
        .await
        .unwrap();
    assert_eq!(sixth_failure.status(), StatusCode::UNAUTHORIZED);
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
