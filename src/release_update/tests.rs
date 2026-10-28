//! Ports `agent/src/releaseUpdate.test.ts` in Retaskable.com.
use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::TempDir;

fn build() -> Build {
    Build {
        os: "win-x64".to_string(),
        environment: crate::release::Environment::Staging,
        version: "1.4.2".to_string(),
        commit: "abc1234".to_string(),
    }
}

const BYTES: &[u8] = b"the next build";
const CLOUD: &str = "https://cloud.test";

fn sha(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}

fn with_ready_suffix(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(".ready");
    PathBuf::from(s)
}

fn download_url() -> String {
    format!("{CLOUD}/download/1.4.3/RetaskableStagingSetup.exe")
}

#[derive(Clone)]
enum Feed {
    Status(u16),
    Json(Value),
}

struct FakeState {
    feed: Feed,
    bytes: Vec<u8>,
    download_calls: AtomicUsize,
}

/// A [`FetchFn`] answering the release feed (any URL containing `/api/agent/release`) from `state`,
/// and the download (the fixed [`download_url`]) with `state.bytes`, counting its calls — a stand-in
/// for `deps.fetch` in the TypeScript agent's own tests, with no real socket involved (`parse_agent_release`
/// requires an https URL, which this crate's fixtures can't terminate TLS for themselves).
fn fake_fetch(state: Arc<FakeState>) -> FetchFn {
    Box::new(move |url: String| {
        let state = state.clone();
        Box::pin(async move {
            if url.contains("/api/agent/release") {
                return Ok(match &state.feed {
                    Feed::Status(s) => FetchResponse {
                        status: *s,
                        body: Vec::new(),
                    },
                    Feed::Json(v) => FetchResponse {
                        status: 200,
                        body: serde_json::to_vec(v).unwrap(),
                    },
                });
            }
            if url == download_url() {
                state.download_calls.fetch_add(1, Ordering::SeqCst);
                return Ok(FetchResponse {
                    status: 200,
                    body: state.bytes.clone(),
                });
            }
            Ok(FetchResponse {
                status: 404,
                body: Vec::new(),
            })
        })
    })
}

/// A fake environment answering the release feed (built by `feed` from the fixed [`download_url`])
/// and the download, serving `bytes`. Gives the fixed cloud URL, a [`FetchFn`] for it, and the state
/// to assert download-call counts against.
fn fake_cloud(bytes: &[u8], feed: impl FnOnce(&str) -> Feed) -> (String, FetchFn, Arc<FakeState>) {
    let state = Arc::new(FakeState {
        feed: feed(&download_url()),
        bytes: bytes.to_vec(),
        download_calls: AtomicUsize::new(0),
    });
    let fetch = fake_fetch(state.clone());
    (CLOUD.to_string(), fetch, state)
}

/// Feeds the newest build "1.4.3" the usual way: a valid feed entry pointing at the fixed download
/// URL, hashed to `bytes`.
fn fake_cloud_with_1_4_3(bytes: &[u8]) -> (String, FetchFn, Arc<FakeState>) {
    fake_cloud(bytes, |download_url| {
        Feed::Json(json!({ "version": "1.4.3", "url": download_url, "sha256": sha(bytes) }))
    })
}

/// A [`ReleaseUpdater`] against `data_dir`, with its logs and ready-callbacks captured.
struct Harness {
    updater: ReleaseUpdater,
    logs: Arc<Mutex<Vec<String>>>,
    readied: Arc<Mutex<Vec<ReadyUpdate>>>,
}

fn harness(cloud_url: &str, fetch: FetchFn, data_dir: &Path, installer: Option<Arc<dyn Installer>>) -> Harness {
    let logs = Arc::new(Mutex::new(Vec::new()));
    let readied = Arc::new(Mutex::new(Vec::new()));
    let (logs_w, readied_w) = (logs.clone(), readied.clone());
    let updater = ReleaseUpdater::new(ReleaseUpdateDeps {
        build: build(),
        cloud_url: cloud_url.to_string(),
        data_dir: data_dir.to_path_buf(),
        http: reqwest::Client::new(),
        installer,
        on_ready: Some(Box::new(move |r: &ReadyUpdate| {
            readied_w.lock().unwrap().push(r.clone())
        })),
        log: Some(Box::new(move |m: &str| logs_w.lock().unwrap().push(m.to_string()))),
        fetch: Some(fetch),
    });
    Harness { updater, logs, readied }
}

fn joined(logs: &Arc<Mutex<Vec<String>>>) -> String {
    logs.lock().unwrap().join("\n")
}

/// An installer readying a download as `<file>.ready` and remembering what it installed.
struct TestInstaller {
    cannot: Option<String>,
    refuse: Option<String>,
    fails: bool,
    installed: Mutex<Vec<PathBuf>>,
}

impl TestInstaller {
    fn new() -> Self {
        Self {
            cannot: None,
            refuse: None,
            fails: false,
            installed: Mutex::new(Vec::new()),
        }
    }

    fn failing() -> Self {
        Self {
            fails: true,
            ..Self::new()
        }
    }

    fn cannot_replace(msg: &str) -> Self {
        Self {
            cannot: Some(msg.to_string()),
            ..Self::new()
        }
    }

    fn refusing(msg: &str) -> Self {
        Self {
            refuse: Some(msg.to_string()),
            ..Self::new()
        }
    }
}

impl Installer for TestInstaller {
    fn cannot(&self) -> Option<String> {
        self.cannot.clone()
    }

    fn prepare(&self, file: &Path) -> Result<PathBuf, String> {
        match &self.refuse {
            Some(refuse) => Err(refuse.clone()),
            None => Ok(with_ready_suffix(file)),
        }
    }

    fn install(&self, prepared: &Path) -> Result<bool, String> {
        if self.fails {
            return Err("the disk is full".to_string());
        }
        self.installed.lock().unwrap().push(prepared.to_path_buf());
        Ok(true)
    }
}

#[test]
fn release_feed_entry_is_a_version_an_https_url_and_a_sha256() {
    let sha = sha(BYTES);
    assert_eq!(
        parse_agent_release(&json!({ "version": "1.4.3", "url": "https://d.test/a.exe", "sha256": sha, "extra": 1 })),
        Some(AgentRelease {
            version: "1.4.3".to_string(),
            url: "https://d.test/a.exe".to_string(),
            sha256: sha.clone(),
        })
    );
    assert_eq!(
        parse_agent_release(&json!({ "version": "1.4.3", "url": "http://d.test/a.exe", "sha256": sha })),
        None
    );
    assert_eq!(
        parse_agent_release(&json!({ "version": "latest", "url": "https://d.test/a.exe", "sha256": sha })),
        None
    );
    assert_eq!(
        parse_agent_release(
            &json!({ "version": "1.4.3", "url": "https://d.test/a.exe", "sha256": sha.to_uppercase() })
        ),
        None
    );
    assert_eq!(
        parse_agent_release(&json!({ "version": "1.4.3", "url": "not a url", "sha256": sha })),
        None
    );
    assert_eq!(parse_agent_release(&Value::Null), None);
    assert_eq!(release_target("mac", "arm64"), Some("mac-arm64"));
    assert_eq!(release_target("linux", "arm64"), None);
}

#[test]
fn versions_compare_number_by_number() {
    assert!(is_newer_version("1.10.0", "1.9.3"));
    assert!(is_newer_version("1.4.3", "1.4.2"));
    assert!(!is_newer_version("1.4.2", "1.4.2"));
    assert!(!is_newer_version("1.4", "1.4.0"));
    assert!(!is_newer_version("1.3.9", "1.4.0"));
    assert!(is_newer_version("2", "1.99.99"));
}

#[test]
fn asked_for_the_builds_own_target_on_its_environment() {
    const STAGING: &str = "https://staging.retaskable.com";
    assert_eq!(
        release_feed_url(STAGING, "win-x64"),
        Some(format!("{STAGING}/api/agent/release?os=win&arch=x64"))
    );
    assert_eq!(
        release_feed_url("https://retaskable.com", "mac-arm64"),
        Some("https://retaskable.com/api/agent/release?os=mac&arch=arm64".to_string())
    );
    assert_eq!(release_feed_url(STAGING, "linux-arm64"), None);
    assert_eq!(
        update_file_name("https://downloads.retaskable.test/agent/1.4.3/RetaskableStagingSetup.exe"),
        "RetaskableStagingSetup.exe"
    );
    assert_eq!(update_file_name("https://d.test/a/..%2F..%2Fevil.exe"), "_.._evil.exe");
    assert_eq!(update_file_name("https://d.test/"), "update");
}

#[tokio::test]
async fn a_newer_build_is_downloaded_into_updates_version_and_checked() {
    let data_dir = TempDir::new().unwrap();
    let (base, fetch, state) = fake_cloud_with_1_4_3(BYTES);
    let h = harness(&base, fetch, data_dir.path(), None);
    let ready = h.updater.check().await;
    let file = data_dir
        .path()
        .join("updates")
        .join("1.4.3")
        .join("RetaskableStagingSetup.exe");
    assert_eq!(
        ready,
        Some(ReadyUpdate {
            version: "1.4.3".to_string(),
            file: file.clone(),
            install: file.clone(),
        })
    );
    assert_eq!(std::fs::read(&file).unwrap(), BYTES);
    assert!(!part_path(&file).exists());
    assert!(joined(&h.logs).contains("1.4.3 downloaded and checked"));
    // The next hour's check finds the same version and downloads nothing more.
    h.updater.check().await;
    assert_eq!(state.download_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_download_whose_sha256_isnt_the_feeds_is_deleted_and_refused() {
    let data_dir = TempDir::new().unwrap();
    let wrong = b"something else";
    let (base, fetch, _state) = fake_cloud(wrong, |download_url| {
        Feed::Json(json!({ "version": "1.4.3", "url": download_url, "sha256": sha(BYTES) }))
    });
    let h = harness(&base, fetch, data_dir.path(), None);
    assert_eq!(h.updater.check().await, None);
    assert_eq!(h.updater.ready(), None);
    assert!(!data_dir.path().join("updates").join("1.4.3").exists());
    assert!(joined(&h.logs).contains("isn't the build the feed names (its SHA-256 differs), so it was refused"));
}

#[tokio::test]
async fn nothing_is_downloaded_for_the_same_or_an_older_version_or_a_404() {
    for feed in [
        Feed::Json(json!({ "version": "1.4.2", "url": "https://x.test/ignored", "sha256": sha(BYTES) })),
        Feed::Json(json!({ "version": "1.3.0", "url": "https://x.test/ignored", "sha256": sha(BYTES) })),
        Feed::Status(404),
    ] {
        let data_dir = TempDir::new().unwrap();
        let (base, fetch, state) = fake_cloud(BYTES, move |_| feed.clone());
        let h = harness(&base, fetch, data_dir.path(), None);
        assert_eq!(h.updater.check().await, None);
        assert_eq!(state.download_calls.load(Ordering::SeqCst), 0);
        assert!(h.logs.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_bad_answer_or_a_missing_field_logs_a_check_failure() {
    for feed in [
        Feed::Status(500),
        Feed::Json(json!({ "version": "1.4.3", "url": "https://x.test/ignored" })),
    ] {
        let data_dir = TempDir::new().unwrap();
        let (base, fetch, state) = fake_cloud(BYTES, move |_| feed.clone());
        let h = harness(&base, fetch, data_dir.path(), None);
        assert_eq!(h.updater.check().await, None);
        assert_eq!(state.download_calls.load(Ordering::SeqCst), 0);
        assert!(joined(&h.logs).contains("couldn't check for a newer version"));
    }
}

#[tokio::test]
async fn an_existing_correct_download_isnt_fetched_again_older_versions_go() {
    let data_dir = TempDir::new().unwrap();
    let (base, fetch, state) = fake_cloud_with_1_4_3(BYTES);
    let old = data_dir.path().join("updates").join("1.4.1");
    std::fs::create_dir_all(&old).unwrap();
    let current = data_dir.path().join("updates").join("1.4.3");
    std::fs::create_dir_all(&current).unwrap();
    std::fs::write(current.join("RetaskableStagingSetup.exe"), BYTES).unwrap();
    let h = harness(&base, fetch, data_dir.path(), None);
    let ready = h.updater.check().await;
    assert_eq!(ready.map(|r| r.version), Some("1.4.3".to_string()));
    assert_eq!(state.download_calls.load(Ordering::SeqCst), 0);
    assert!(!old.exists());
}

#[tokio::test]
async fn a_ready_build_asks_for_the_restart_and_the_restarts_exit_installs_it() {
    let data_dir = TempDir::new().unwrap();
    let (base, fetch, _state) = fake_cloud_with_1_4_3(BYTES);
    let installer = Arc::new(TestInstaller::new());
    let h = harness(
        &base,
        fetch,
        data_dir.path(),
        Some(installer.clone() as Arc<dyn Installer>),
    );
    assert!(!h.updater.install_now(), "nothing ready yet");
    let file = data_dir
        .path()
        .join("updates")
        .join("1.4.3")
        .join("RetaskableStagingSetup.exe");
    let install = with_ready_suffix(&file);
    let ready = h.updater.check().await;
    assert_eq!(
        ready,
        Some(ReadyUpdate {
            version: "1.4.3".to_string(),
            file: file.clone(),
            install: install.clone(),
        })
    );
    assert_eq!(
        h.readied
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.version.clone())
            .collect::<Vec<_>>(),
        vec!["1.4.3".to_string()]
    );
    assert!(h.updater.install_now());
    assert_eq!(*installer.installed.lock().unwrap(), vec![install]);
    assert!(data_dir.path().join("updates").join("1.4.3").join(TRIED_FILE).exists());
    assert!(joined(&h.logs).contains("installing 1.4.3"));
}

#[tokio::test]
async fn a_failed_install_isnt_tried_again_after_a_restart() {
    let data_dir = TempDir::new().unwrap();
    let (base, fetch, _state) = fake_cloud_with_1_4_3(BYTES);
    let h = harness(
        &base,
        fetch,
        data_dir.path(),
        Some(Arc::new(TestInstaller::failing()) as Arc<dyn Installer>),
    );
    h.updater.check().await;
    assert!(!h.updater.install_now());
    assert!(joined(&h.logs).contains("couldn't install 1.4.3 (the disk is full), so this copy starts again"));

    // The next start, still 1.4.2: a fresh updater against the same data dir doesn't download or ask again.
    let (base2, fetch2, _state2) = fake_cloud_with_1_4_3(BYTES);
    let logs = h.logs.clone();
    let again = ReleaseUpdater::new(ReleaseUpdateDeps {
        build: build(),
        cloud_url: base2,
        data_dir: data_dir.path().to_path_buf(),
        http: reqwest::Client::new(),
        installer: Some(Arc::new(TestInstaller::new())),
        on_ready: Some(Box::new(|_: &ReadyUpdate| panic!("no restart expected"))),
        log: Some(Box::new(move |m: &str| logs.lock().unwrap().push(m.to_string()))),
        fetch: Some(fetch2),
    });
    assert_eq!(again.check().await, None);
    again.check().await;
    let said = h
        .logs
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.contains("isn't tried again"))
        .count();
    assert_eq!(said, 1, "said once");
}

#[tokio::test]
async fn a_copy_that_cant_replace_itself_or_a_refused_signature_asks_for_no_restart() {
    let data_dir = TempDir::new().unwrap();
    let (base, fetch, state) = fake_cloud_with_1_4_3(BYTES);
    let cannot = Arc::new(TestInstaller::cannot_replace("it isn't running as an AppImage"));
    let h = harness(&base, fetch, data_dir.path(), Some(cannot as Arc<dyn Installer>));
    assert_eq!(h.updater.check().await, None);
    assert_eq!(state.download_calls.load(Ordering::SeqCst), 0);
    assert!(
        joined(&h.logs).contains("1.4.3 is out, but this copy can't replace itself (it isn't running as an AppImage)")
    );

    let data_dir2 = TempDir::new().unwrap();
    let (base2, fetch2, _state2) = fake_cloud_with_1_4_3(BYTES);
    let refusing = Arc::new(TestInstaller::refusing(
        "the download was refused: it's signed by CN=Mallory",
    ));
    let h2 = harness(&base2, fetch2, data_dir2.path(), Some(refusing as Arc<dyn Installer>));
    assert_eq!(h2.updater.check().await, None);
    assert!(h2.readied.lock().unwrap().is_empty());
    assert!(!data_dir2.path().join("updates").join("1.4.3").exists());
    assert!(joined(&h2.logs).contains("signed by CN=Mallory"));
}
