//! The page's secrets (agent-settings.md sections 3.3 and 4.3): states without values, saves only from this computer.
use std::net::IpAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::http::StatusCode;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::test_support::*;
use super::*;
use crate::password;
use crate::secrets::fakes::{FakeHost, MemStore};
use crate::secrets::{Host, Secrets};

const PLANTED: &str = "ghp_PLANTED_http_secret_0123456789";
const PEER: &str = "172.17.0.1";

struct Rig {
    dir: tempfile::TempDir,
    host: Arc<FakeHost>,
    settings: Arc<SettingsServer>,
}

/// A settings server over a fresh folder with a `Secrets` of fakes; `os` None is a machine with no OS store, `key` the
/// headless key variable's value.
fn rig(os: bool, key: Option<&str>, settings_env: &[(&str, &str)], host_env: &[(&str, &str)], network: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    if network {
        MachineStore::load(dir.path())
            .unwrap()
            .set_network(NetworkAccess {
                enabled: true,
                password_hash: Some(password::hash_password("a good enough password")),
            })
            .unwrap();
    }
    let host = Arc::new(FakeHost::with(host_env));
    let key_env = key
        .map(|k| vec![("RETASKABLE_SECRETS_KEY", k.to_string())])
        .unwrap_or_default();
    let os: Option<Arc<dyn TokenStore>> = os.then(|| Arc::new(MemStore::default()) as Arc<dyn TokenStore>);
    let secrets = Secrets::new(
        dir.path(),
        &Env::from_pairs(key_env),
        host.clone(),
        os,
        Arc::new(MemStore::default()),
    );
    secrets.apply();
    let server = SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        4700,
        network,
        Env::from_pairs(settings_env.iter().copied()),
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(dir.path()),
    );
    server.set_secrets(Arc::new(secrets));
    Rig {
        dir,
        host,
        settings: Arc::new(server),
    }
}

/// Signs in with the password from another computer (a token signs in from this one only).
async fn sign_in_remote(app: &Router) -> (String, String) {
    let res = app
        .clone()
        .oneshot(post_body(
            "/api/settings/session",
            &json!({ "password": "a good enough password" }).to_string(),
        ))
        .await
        .unwrap();
    let cookie = res
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    (cookie, json_body(res).await["csrfToken"].as_str().unwrap().to_string())
}

async fn text_of(res: axum::response::Response) -> (StatusCode, String) {
    let status = res.status();
    (
        status,
        String::from_utf8(res.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap(),
    )
}

async fn get(app: &Router, cookie: &str) -> (StatusCode, String) {
    let req = request("GET", "/api/settings")
        .header("Cookie", cookie)
        .body(Body::empty())
        .unwrap();
    text_of(app.clone().oneshot(req).await.unwrap()).await
}

async fn post(app: &Router, cookie: &str, csrf: &str, body: Value) -> (StatusCode, String) {
    text_of(post_setting(app, cookie, csrf, body).await).await
}

fn secret<'a>(view: &'a Value, name: &str) -> &'a Value {
    view["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap()
}

#[tokio::test]
async fn the_page_lists_each_secret_with_its_state_and_can_save_is_this_computer() {
    let r = rig(true, None, &[], &[("GITLAB_TOKEN", "glpat-env")], false);
    let view = signed_in_settings(&r.settings).await;
    assert_eq!(view["canSaveSecrets"], true);
    assert_eq!(view["secretsNote"], Value::Null);
    let names: Vec<&str> = view["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "GH_TOKEN",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "ANTHROPIC_API_KEY",
            "GITLAB_TOKEN",
            "CLOUDFLARE_ACCOUNT_ID",
            "CLOUDFLARE_API_TOKEN"
        ]
    );
    assert_eq!(secret(&view, "GH_TOKEN")["source"], "unset");
    assert!(secret(&view, "GH_TOKEN").get("fingerprint").is_none());
    assert_eq!(secret(&view, "GITLAB_TOKEN")["source"], "env");
}

#[tokio::test]
async fn a_saved_secret_shows_saved_with_its_fingerprint_and_never_the_value_anywhere() {
    let r = rig(true, None, &[], &[], false);
    let app = app(r.settings.clone());
    let (cookie, csrf) = sign_in(&app, &r.settings).await;
    let (status, saved_body) = post(&app, &cookie, &csrf, json!({ "secrets": { "GH_TOKEN": PLANTED } })).await;
    assert_eq!(status, StatusCode::OK, "{saved_body}");
    let (_, got) = get(&app, &cookie).await;
    let view: Value = serde_json::from_str(&got).unwrap();
    assert_eq!(secret(&view, "GH_TOKEN")["source"], "saved");
    assert_eq!(
        secret(&view, "GH_TOKEN")["fingerprint"],
        crate::secrets::fingerprint(PLANTED)
    );
    assert_eq!(r.host.get("GH_TOKEN").as_deref(), Some(PLANTED));
    // Every response, a status poll, a refused save, a bad value and the config on disk: no plaintext.
    let (_, refused) = post(
        &app,
        &cookie,
        &csrf,
        json!({ "secrets": { "GH_TOKEN": format!("{PLANTED} spaced") } }),
    )
    .await;
    let (_, nonsense) = post(&app, &cookie, &csrf, json!({ "secrets": { PLANTED: PLANTED } })).await;
    let (_, wrong_type) = post(&app, &cookie, &csrf, json!({ "secrets": [PLANTED] })).await;
    let status_poll = text_of(
        app.clone()
            .oneshot(
                request("GET", "/api/settings/status")
                    .header("Cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap(),
    )
    .await
    .1;
    let on_disk = std::fs::read_to_string(r.dir.path().join("config.json")).unwrap();
    for (what, text) in [
        ("save", &saved_body),
        ("GET", &got),
        ("refused", &refused),
        ("bad name", &nonsense),
        ("not an object", &wrong_type),
        ("status", &status_poll),
        ("config.json", &on_disk),
    ] {
        assert!(!text.contains(PLANTED), "{what} carried the value: {text}");
    }
    // Clearing from the page.
    let (status, _) = post(&app, &cookie, &csrf, json!({ "secrets": { "GH_TOKEN": null } })).await;
    assert_eq!(status, StatusCode::OK);
    let view: Value = serde_json::from_str(&get(&app, &cookie).await.1).unwrap();
    assert_eq!(secret(&view, "GH_TOKEN")["source"], "unset");
    assert_eq!(r.host.get("GH_TOKEN"), None);
}

#[tokio::test]
async fn a_secret_the_environment_sets_is_not_locked_a_save_wins_and_a_reset_goes_back_to_it() {
    let dir_env = [("GH_TOKEN", "from-env")];
    let dir = tempfile::tempdir().unwrap();
    let host = Arc::new(FakeHost::with(&dir_env));
    let secrets = Secrets::new(
        dir.path(),
        &Env::empty(),
        host.clone(),
        Some(Arc::new(MemStore::default())),
        Arc::new(MemStore::default()),
    );
    secrets.apply();
    let server = SettingsServer::new(
        dir.path().to_path_buf(),
        4700,
        4700,
        false,
        Env::empty(),
        || 1_700_000_000,
        |_: i32| {},
        test_keychain(dir.path()),
    );
    server.set_secrets(Arc::new(secrets));
    let settings = Arc::new(server);
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;
    let view: Value = serde_json::from_str(&get(&app, &cookie).await.1).unwrap();
    assert_eq!(secret(&view, "GH_TOKEN")["source"], "env");
    assert_eq!(
        secret(&view, "GH_TOKEN")["fingerprint"],
        crate::secrets::fingerprint("from-env")
    );
    assert_eq!(secret(&view, "GH_TOKEN")["envSet"], true);
    let (status, _) = post(&app, &cookie, &csrf, json!({ "secrets": { "GH_TOKEN": "saved-wins" } })).await;
    assert_eq!(status, StatusCode::OK);
    let view: Value = serde_json::from_str(&get(&app, &cookie).await.1).unwrap();
    assert_eq!(secret(&view, "GH_TOKEN")["source"], "saved");
    assert_eq!(
        secret(&view, "GH_TOKEN")["envSet"],
        true,
        "Reset says what it goes back to"
    );
    assert_eq!(host.get("GH_TOKEN").as_deref(), Some("saved-wins"));
    // Reset: the saved one is deleted and the environment's value is back.
    let (status, _) = post(&app, &cookie, &csrf, json!({ "secrets": { "GH_TOKEN": null } })).await;
    assert_eq!(status, StatusCode::OK);
    let view: Value = serde_json::from_str(&get(&app, &cookie).await.1).unwrap();
    assert_eq!(secret(&view, "GH_TOKEN")["source"], "env");
    assert_eq!(host.get("GH_TOKEN").as_deref(), Some("from-env"));
}

#[tokio::test]
async fn nothing_is_saved_when_another_setting_in_the_same_body_is_refused() {
    let r = rig(true, None, &[], &[], false);
    let app = app(r.settings.clone());
    let (cookie, csrf) = sign_in(&app, &r.settings).await;
    let (status, _) = post(
        &app,
        &cookie,
        &csrf,
        json!({ "port": 80, "secrets": { "GH_TOKEN": PLANTED } }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(r.host.get("GH_TOKEN"), None);
    let on_disk = std::fs::read_to_string(r.dir.path().join("config.json")).unwrap_or_default();
    assert!(!on_disk.contains("secrets"));
    // And a good body saves both.
    let (status, _) = post(
        &app,
        &cookie,
        &csrf,
        json!({ "pollIntervalSec": 30, "secrets": { "GH_TOKEN": PLANTED } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(r.host.get("GH_TOKEN").as_deref(), Some(PLANTED));
}

#[tokio::test]
async fn a_save_from_another_computer_is_refused_and_the_page_says_it_cannot_save() {
    let r = rig(true, None, &[], &[], true);
    let peer: IpAddr = PEER.parse().unwrap();
    let remote = app_from(r.settings.clone(), peer);
    let (cookie, csrf) = sign_in_remote(&remote).await;
    let view: Value = serde_json::from_str(&get(&remote, &cookie).await.1).unwrap();
    assert_eq!(view["canSaveSecrets"], false);
    assert!(view["secrets"].is_array(), "the states are still shown");
    let (status, body) = post(&remote, &cookie, &csrf, json!({ "secrets": { "GH_TOKEN": PLANTED } })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("only on this computer") && !body.contains(PLANTED));
    assert_eq!(r.host.get("GH_TOKEN"), None);
    // Clearing is a change too.
    let (status, _) = post(&remote, &cookie, &csrf, json!({ "secrets": { "GH_TOKEN": null } })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Other settings still save from there.
    let (status, _) = post(&remote, &cookie, &csrf, json!({ "pollIntervalSec": 30 })).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_published_port_counts_as_this_computer_only_with_the_flag_and_a_loopback_host() {
    let peer: IpAddr = PEER.parse().unwrap();
    let flagged = rig(true, None, &[("RETASKABLE_SETTINGS_PUBLISHED_LOCAL", "1")], &[], true);
    let remote = app_from(flagged.settings.clone(), peer);
    let (cookie, csrf) = sign_in_remote(&remote).await;
    let view: Value = serde_json::from_str(&get(&remote, &cookie).await.1).unwrap();
    assert_eq!(view["canSaveSecrets"], true);
    let (status, _) = post(&remote, &cookie, &csrf, json!({ "secrets": { "GH_TOKEN": PLANTED } })).await;
    assert_eq!(status, StatusCode::OK);

    // A Host that is this computer's network address is not enough, even with the flag.
    let mut lan = Arc::try_unwrap(rig(true, None, &[("RETASKABLE_SETTINGS_PUBLISHED_LOCAL", "1")], &[], true).settings)
        .ok()
        .unwrap();
    set_own_addresses(&mut lan, vec!["192.168.1.20".parse().unwrap()]);
    let lan = Arc::new(lan);
    assert!(!lan.is_this_computer(peer, &lan_headers("192.168.1.20:4700")));
    assert!(lan.is_this_computer(peer, &lan_headers("localhost:4700")));
    assert!(lan.is_this_computer(peer, &lan_headers("[::1]:4700")));
    assert!(!lan.is_this_computer(peer, &lan_headers("evil.example:4700")));

    // Without the flag, or with it set to anything but 1, a loopback Host from elsewhere is not this computer.
    for env in [
        &[][..],
        &[("RETASKABLE_SETTINGS_PUBLISHED_LOCAL", "0")],
        &[("RETASKABLE_SETTINGS_PUBLISHED_LOCAL", "true")],
    ] {
        let r = rig(true, None, env, &[], true);
        assert!(!r.settings.is_this_computer(peer, &lan_headers("127.0.0.1:4700")));
    }
}

fn lan_headers(host: &str) -> axum::http::HeaderMap {
    let mut h = axum::http::HeaderMap::new();
    h.insert(header::HOST, host.parse().unwrap());
    h
}

#[tokio::test]
async fn with_no_os_store_and_no_key_the_page_says_secrets_cannot_be_saved() {
    let r = rig(false, None, &[], &[], false);
    let app = app(r.settings.clone());
    let (cookie, csrf) = sign_in(&app, &r.settings).await;
    let view: Value = serde_json::from_str(&get(&app, &cookie).await.1).unwrap();
    assert_eq!(
        view["canSaveSecrets"], true,
        "this computer: the key is what is missing"
    );
    let note = view["secretsNote"].as_str().unwrap();
    assert!(note.contains("RETASKABLE_SECRETS_KEY_FILE"), "{note}");
    let (status, body) = post(&app, &cookie, &csrf, json!({ "secrets": { "GH_TOKEN": PLANTED } })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body.contains("RETASKABLE_SECRETS_KEY_FILE") && !body.contains(PLANTED));
    assert_eq!(r.host.get("GH_TOKEN"), None);
}

#[tokio::test]
async fn a_headless_key_lets_the_page_save() {
    let key = crate::secrets::DataKey::generate().to_base64();
    let r = rig(false, Some(&key), &[], &[], false);
    let app = app(r.settings.clone());
    let (cookie, csrf) = sign_in(&app, &r.settings).await;
    let (status, _) = post(
        &app,
        &cookie,
        &csrf,
        json!({ "secrets": { "ANTHROPIC_API_KEY": PLANTED } }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let view: Value = serde_json::from_str(&get(&app, &cookie).await.1).unwrap();
    assert_eq!(view["secretsNote"], Value::Null);
    assert_eq!(secret(&view, "ANTHROPIC_API_KEY")["source"], "saved");
}

#[tokio::test]
async fn an_unreadable_secret_is_shown_as_such_with_no_fingerprint() {
    let r = rig(true, None, &[], &[], false);
    config::save_machine_settings(r.dir.path(), |o| {
        o.insert(
            "secrets".into(),
            json!({ "GH_TOKEN": "v1:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA" }),
        );
    })
    .unwrap();
    let view = signed_in_settings(&r.settings).await;
    assert_eq!(secret(&view, "GH_TOKEN")["source"], "unreadable");
    assert!(secret(&view, "GH_TOKEN").get("fingerprint").is_none());
}

#[tokio::test]
async fn without_secrets_given_the_page_has_none_and_refuses_a_save() {
    let dir = tempfile::tempdir().unwrap();
    let settings = test_server_with_keychain(dir.path(), test_keychain(dir.path()));
    let view = signed_in_settings(&settings).await;
    assert!(view.get("secrets").is_none());
    let app = app(settings.clone());
    let (cookie, csrf) = sign_in(&app, &settings).await;
    let (status, _) = post(&app, &cookie, &csrf, json!({ "secrets": { "GH_TOKEN": "x" } })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
