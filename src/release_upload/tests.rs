use std::cell::RefCell;

use super::*;
use crate::release_update::parse_agent_release;

fn setup(environment: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let bundle = dir.path().join(format!("{environment}-linux-x64"));
    std::fs::create_dir_all(&bundle).unwrap();
    std::fs::write(
        bundle.join("build.json"),
        format!(r#"{{"os":"linux-x64","environment":"{environment}","version":"0.1.812","commit":"abc1234"}}"#),
    )
    .unwrap();
    let packages = dir.path().join("packages");
    std::fs::create_dir_all(&packages).unwrap();
    let name = if environment == "staging" {
        "Retaskable-Staging-x86_64.AppImage"
    } else {
        "Retaskable-x86_64.AppImage"
    };
    std::fs::write(packages.join(name), b"appimage").unwrap();
    dir
}

#[derive(Default)]
struct Fake {
    puts: RefCell<Vec<(String, Vec<u8>, String)>>,
    fail_on: Option<&'static str>,
}

impl Put for Fake {
    fn put(&self, key: &str, body: Vec<u8>, content_type: &str) -> Result<(), String> {
        if self.fail_on.is_some_and(|k| key.contains(k)) {
            return Err(format!("couldn't put {key}"));
        }
        self.puts
            .borrow_mut()
            .push((key.to_string(), body, content_type.to_string()));
        Ok(())
    }
}

#[test]
fn linux_x64_key_type_and_entry_with_url_and_hash() {
    let dir = setup("staging");
    let (uploads, skipped) = release_uploads(Environment::Staging, dir.path(), &dir.path().join("packages")).unwrap();
    assert_eq!(uploads.len(), 1);
    let u = &uploads[0];
    assert_eq!(u.key, "downloads/0.1.812/Retaskable-Staging-x86_64.AppImage");
    assert_eq!(u.content_type, "application/vnd.appimage");
    assert_eq!(
        u.entry["url"],
        "https://staging.retaskable.com/api/agent/download/0.1.812/Retaskable-Staging-x86_64.AppImage"
    );
    // sha256("appimage")
    assert_eq!(u.entry["sha256"].as_str().unwrap().len(), 64);
    let want: String = Sha256::digest(b"appimage").iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(u.entry["sha256"], want.as_str());
    let parsed = parse_agent_release(&u.entry).unwrap();
    assert_eq!(parsed.version, "0.1.812");
    assert_eq!(
        skipped,
        [
            "win-x64: the desktop app's",
            "mac-arm64: the desktop app's",
            "mac-x64: the desktop app's"
        ]
    );
}

#[test]
fn production_uses_its_own_origin() {
    let dir = setup("production");
    let (uploads, _) = release_uploads(Environment::Production, dir.path(), &dir.path().join("packages")).unwrap();
    assert!(
        uploads[0].entry["url"]
            .as_str()
            .unwrap()
            .starts_with("https://retaskable.com/api/agent/download/0.1.812/")
    );
}

#[test]
fn a_missing_package_or_bundle_is_skipped_and_named() {
    let dir = setup("staging");
    let (uploads, skipped) = release_uploads(Environment::Staging, dir.path(), &dir.path().join("none")).unwrap();
    assert!(uploads.is_empty());
    assert!(skipped.contains(&"linux-x64: Retaskable-Staging-x86_64.AppImage isn't packaged".to_string()));
    let (uploads, skipped) = release_uploads(Environment::Production, dir.path(), dir.path()).unwrap();
    assert!(uploads.is_empty());
    assert!(skipped.contains(&"linux-x64: no bundle".to_string()));
}

#[test]
fn the_package_is_put_before_its_entry() {
    let dir = setup("staging");
    let (uploads, _) = release_uploads(Environment::Staging, dir.path(), &dir.path().join("packages")).unwrap();
    let fake = Fake::default();
    publish(&uploads, &fake).unwrap();
    let puts = fake.puts.borrow();
    assert_eq!(puts[0].0, "downloads/0.1.812/Retaskable-Staging-x86_64.AppImage");
    assert_eq!(puts[0].1, b"appimage");
    assert_eq!(puts[1].0, "linux-x64.json");
    assert_eq!(puts[1].2, "application/json");
    let entry: serde_json::Value = serde_json::from_slice(&puts[1].1).unwrap();
    assert!(parse_agent_release(&entry).is_some());
}

#[test]
fn a_failed_package_put_writes_no_entry() {
    let dir = setup("staging");
    let (uploads, _) = release_uploads(Environment::Staging, dir.path(), &dir.path().join("packages")).unwrap();
    let fake = Fake {
        fail_on: Some("downloads/"),
        ..Default::default()
    };
    assert!(publish(&uploads, &fake).is_err());
    assert!(fake.puts.borrow().is_empty());
}

#[test]
fn missing_credentials_fail_clearly() {
    let err = R2::from_env(Environment::Staging, |k| {
        (k == "CLOUDFLARE_ACCOUNT_ID").then(|| "acc".to_string())
    })
    .err()
    .unwrap();
    assert!(err.contains("CLOUDFLARE_API_TOKEN isn't set"), "{err}");
    let err = R2::from_env(Environment::Staging, |_| Some("  ".to_string()))
        .err()
        .unwrap();
    assert!(err.contains("isn't set"), "{err}");
    let ok = R2::from_env(Environment::Production, |_| Some("x".to_string())).unwrap();
    assert_eq!(ok.bucket(), "retaskable-agent-releases-production");
}
