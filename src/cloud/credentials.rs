//! Storing the refresh token a device link gets (cloud/04-authentication.md section 4.2, step 5):
//! "the OS credential store: Windows Credential Manager, macOS Keychain, or the Secret Service on
//! Linux ... Never in a plain file, and never in `.retaskable/`, except on a machine with no OS
//! credential store at all (a Docker container)". docs/specs/docker.md section 4 is the fallback's
//! spec: on Linux, probe once; if that throws, every secret (not just this one) goes in
//! `.retaskable/credentials.json`, mode 0600, by service and account as the OS store keeps them.
//! Windows and macOS always use the OS store. On Linux the `keyring` crate's `v1` module uses only
//! the Secret Service (not the kernel keyring), so a headless box without Secret Service takes the
//! same file fallback a container does.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

/// docker.md section 4's "a probe entry": read once, so the one failure that means there's no OS
/// credential store at all is told apart from an ordinary "nothing stored under this account yet".
const PROBE_SERVICE: &str = "retaskable-probe";
const PROBE_ACCOUNT: &str = "probe";

/// The service name every secret is kept under, in the OS store and in the file fallback alike
/// (cloud/04-authentication.md: "service `retaskable`, account the cloud URL").
pub const SERVICE: &str = "retaskable";

/// The OS credential store, behind a trait so a test can stand in for it instead of touching the
/// real Windows Credential Manager, macOS Keychain or Linux Secret Service.
pub trait Keyring {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>>;
    fn set(&self, service: &str, account: &str, secret: &str) -> Result<()>;
}

/// The real OS credential store, through the `keyring` crate.
pub struct OsKeyring;

impl Keyring for OsKeyring {
    fn get(&self, service: &str, account: &str) -> Result<Option<String>> {
        let entry = keyring::Entry::new(service, account)?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn set(&self, service: &str, account: &str, secret: &str) -> Result<()> {
        keyring::Entry::new(service, account)?.set_password(secret)?;
        Ok(())
    }
}

/// Where this run's secrets actually live.
enum Backing {
    Os,
    File(PathBuf),
}

/// One machine's secrets for this run, keyed by service and account.
pub struct CredentialStore {
    backing: Backing,
}

impl CredentialStore {
    /// Probes the OS store once and decides where secrets live for this run. `is_linux` is a
    /// parameter rather than read from `cfg!`, so a test can exercise the fallback on any
    /// platform (`session.rs`'s `resolve_claude` does the same for its `windows` parameter):
    /// docker.md section 4 says only Linux ever lacks one; Windows and macOS always have it.
    pub fn detect(keyring: &dyn Keyring, dir: &Path, is_linux: bool) -> Self {
        if is_linux && keyring.get(PROBE_SERVICE, PROBE_ACCOUNT).is_err() {
            let path = dir.join("credentials.json");
            tracing::warn!(
                "No OS credential store here: link tokens and keys are kept in {}, readable by this user only.",
                path.display()
            );
            return Self {
                backing: Backing::File(path),
            };
        }
        Self { backing: Backing::Os }
    }

    pub fn get(&self, keyring: &dyn Keyring, account: &str) -> Result<Option<String>> {
        match &self.backing {
            Backing::Os => keyring.get(SERVICE, account),
            Backing::File(path) => get_from_file(path, account),
        }
    }

    pub fn set(&self, keyring: &dyn Keyring, account: &str, secret: &str) -> Result<()> {
        match &self.backing {
            Backing::Os => keyring.set(SERVICE, account, secret),
            Backing::File(path) => set_in_file(path, account, secret),
        }
    }
}

fn read_map(path: &Path) -> Result<Map<String, Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str(&text)? {
            Value::Object(map) => Ok(map),
            // "A file that isn't JSON is an error, never read as empty" (a write would wipe the
            // other tokens it holds).
            _ => bail!("{} isn't a JSON object", path.display()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn get_from_file(path: &Path, account: &str) -> Result<Option<String>> {
    let map = read_map(path)?;
    Ok(map
        .get(SERVICE)
        .and_then(|v| v.get(account))
        .and_then(Value::as_str)
        .map(str::to_string))
}

/// Writes the whole file atomically, by service and account as the OS store would be, so other
/// secrets already there (docker.md section 4: "every secret ... goes in
/// `.retaskable/credentials.json` instead") round-trip untouched.
fn set_in_file(path: &Path, account: &str, secret: &str) -> Result<()> {
    let mut map = read_map(path)?;
    let service = map
        .entry(SERVICE.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(accounts) = service else {
        bail!("{} isn't a JSON object", path.display());
    };
    accounts.insert(account.to_string(), Value::String(secret.to_string()));
    write_atomic_0600(path, &Value::Object(map))
}

/// A temp file created mode 0600, then renamed over: never half written, never readable by
/// another user even for an instant. Writing a fresh temp file each time is what "tightens a
/// looser mode" (docker.md section 4): the renamed-over file's mode is always the new one's.
fn write_atomic_0600(path: &Path, data: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let tmp_name = format!(
        "{}.tmp",
        path.file_name()
            .context("credentials file has no name")?
            .to_string_lossy()
    );
    let tmp = path.with_file_name(tmp_name);
    // Drop a leftover temp so OpenOptions::mode applies (it only affects newly created files).
    let _ = std::fs::remove_file(&tmp);
    write_new_0600(&tmp, &serde_json::to_string_pretty(data)?)?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

/// Creates `path` with mode 0600 before any bytes are written, so the secret is never world-readable
/// even briefly (unlike write-then-chmod).
#[cfg(unix)]
fn write_new_0600(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_new_0600(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents).with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct FakeKeyring {
        store: RefCell<HashMap<(String, String), String>>,
        probe_fails: bool,
    }

    impl FakeKeyring {
        fn new(probe_fails: bool) -> Self {
            Self {
                store: RefCell::new(HashMap::new()),
                probe_fails,
            }
        }
    }

    impl Keyring for FakeKeyring {
        fn get(&self, service: &str, account: &str) -> Result<Option<String>> {
            if self.probe_fails && service == PROBE_SERVICE {
                bail!("no secret service here");
            }
            Ok(self
                .store
                .borrow()
                .get(&(service.to_string(), account.to_string()))
                .cloned())
        }

        fn set(&self, service: &str, account: &str, secret: &str) -> Result<()> {
            self.store
                .borrow_mut()
                .insert((service.to_string(), account.to_string()), secret.to_string());
            Ok(())
        }
    }

    #[test]
    fn detect_never_probes_off_linux_even_when_the_store_would_fail() {
        let keyring = FakeKeyring::new(true);
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::detect(&keyring, dir.path(), false);
        store.set(&keyring, "https://retaskable.com", "tok").unwrap();
        assert!(!dir.path().join("credentials.json").exists());
        assert_eq!(
            store.get(&keyring, "https://retaskable.com").unwrap(),
            Some("tok".to_string())
        );
    }

    #[test]
    fn detect_uses_the_os_store_on_linux_when_the_probe_succeeds() {
        let keyring = FakeKeyring::new(false);
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::detect(&keyring, dir.path(), true);
        store.set(&keyring, "https://retaskable.com", "tok").unwrap();
        assert!(!dir.path().join("credentials.json").exists());
    }

    #[test]
    fn detect_falls_back_to_the_file_on_linux_when_the_probe_fails() {
        let keyring = FakeKeyring::new(true);
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::detect(&keyring, dir.path(), true);
        store.set(&keyring, "https://retaskable.com", "tok").unwrap();
        let path = dir.path().join("credentials.json");
        assert!(path.exists());
        assert_eq!(
            store.get(&keyring, "https://retaskable.com").unwrap(),
            Some("tok".to_string())
        );
        // Never reached the OS store at all, since this account lives only in the file.
        assert_eq!(keyring.get(SERVICE, "https://retaskable.com").unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_written_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let keyring = FakeKeyring::new(true);
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::detect(&keyring, dir.path(), true);
        store.set(&keyring, "https://retaskable.com", "tok").unwrap();
        let path = dir.path().join("credentials.json");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn the_file_fallback_keeps_other_accounts_and_services_untouched() {
        let keyring = FakeKeyring::new(true);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.json");
        std::fs::write(
            &path,
            serde_json::json!({ "retaskable": { "https://staging.retaskable.com": "staging-tok" }, "other-service": { "a": "b" } })
                .to_string(),
        )
        .unwrap();

        let store = CredentialStore::detect(&keyring, dir.path(), true);
        store.set(&keyring, "https://retaskable.com", "prod-tok").unwrap();

        assert_eq!(
            store.get(&keyring, "https://staging.retaskable.com").unwrap(),
            Some("staging-tok".to_string())
        );
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved["other-service"]["a"], "b");
    }

    #[test]
    fn get_from_a_missing_file_is_none_not_an_error() {
        let keyring = FakeKeyring::new(true);
        let dir = tempfile::tempdir().unwrap();
        let store = CredentialStore::detect(&keyring, dir.path(), true);
        assert_eq!(store.get(&keyring, "https://retaskable.com").unwrap(), None);
    }

    #[test]
    fn a_non_json_file_is_an_error_not_read_as_empty() {
        let keyring = FakeKeyring::new(true);
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("credentials.json"), "not json").unwrap();
        let store = CredentialStore::detect(&keyring, dir.path(), true);
        assert!(store.get(&keyring, "https://retaskable.com").is_err());
        assert!(store.set(&keyring, "https://retaskable.com", "tok").is_err());
    }
}
