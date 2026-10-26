//! Where the agent keeps its secrets (Retaskable.com agent/src/credentials.ts): the OS credential store (Windows
//! Credential Manager, macOS Keychain, the Secret Service on Linux), or, on a machine with none that works, such as
//! a Docker container (docker.md section 4), one file in the data folder readable by this user only.
use std::collections::BTreeMap;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

/// Where refresh tokens live, keyed by cloud URL (link.ts `TokenStore`).
pub trait TokenStore: Send + Sync {
    fn get(&self, url: &str) -> Result<Option<String>>;
    fn set(&self, url: &str, token: &str) -> Result<()>;
    fn delete(&self, url: &str) -> Result<()>;
}

/// The OS credential store's service name for the link tokens (link.ts `KEYCHAIN_SERVICE`).
const KEYCHAIN_SERVICE: &str = "retaskable";

/// The file used when there's no OS credential store: `<data dir>/credentials.json`.
pub fn credentials_file(data_dir: &Path) -> PathBuf {
    data_dir.join("credentials.json")
}

/// The link tokens' store (link.ts `keychain`): in the file `RETASKABLE_CREDENTIALS_FILE` names, else in the OS
/// credential store when it works, else in the data folder's file. The override keeps an agent started to check a
/// build off this computer's own link (credentials.ts `credentialsFileOverride`).
pub fn keychain(data_dir: &Path, env: impl Fn(&str) -> Option<String>) -> Arc<dyn TokenStore> {
    if let Some(file) = env("RETASKABLE_CREDENTIALS_FILE").filter(|f| !f.trim().is_empty()) {
        return Arc::new(FileStore::new(PathBuf::from(file.trim()), KEYCHAIN_SERVICE));
    }
    if os_store_works() {
        Arc::new(OsStore(KEYCHAIN_SERVICE))
    } else {
        Arc::new(FileStore::new(credentials_file(data_dir), KEYCHAIN_SERVICE))
    }
}

/// The line at start on a machine using a file, else None (credentials.ts `credentialFileNote`).
pub fn credential_file_note(data_dir: &Path, env: impl Fn(&str) -> Option<String>) -> Option<String> {
    if let Some(file) = env("RETASKABLE_CREDENTIALS_FILE").filter(|f| !f.trim().is_empty()) {
        return Some(format!(
            "Link tokens and keys are kept in {} (RETASKABLE_CREDENTIALS_FILE), not the OS credential store.",
            file.trim()
        ));
    }
    (!os_store_works()).then(|| {
        format!(
            "No OS credential store here: link tokens and keys are kept in {}, readable by this user only.",
            credentials_file(data_dir).display()
        )
    })
}

/// Whether this machine's OS credential store answers: a read of an entry nobody writes. Always yes on Windows and
/// macOS. On Linux, in a Docker container there's no Secret Service, so the read fails.
fn os_store_works() -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    match keyring::Entry::new("retaskable-probe", "probe").and_then(|e| e.get_password()) {
        Ok(_) | Err(keyring::Error::NoEntry) => true,
        Err(_) => false,
    }
}

/// The OS credential store, one entry per URL under the service. Its Windows target is `<url>.<service>`, the same as
/// the TypeScript agent's, so either agent finds the other's link.
struct OsStore(&'static str);

impl TokenStore for OsStore {
    fn get(&self, url: &str) -> Result<Option<String>> {
        match keyring::Entry::new(self.0, url).and_then(|e| e.get_password()) {
            Ok(token) => Ok(Some(token).filter(|t| !t.is_empty())),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e).context("reading the OS credential store"),
        }
    }
    fn set(&self, url: &str, token: &str) -> Result<()> {
        keyring::Entry::new(self.0, url)
            .and_then(|e| e.set_password(token))
            .context("writing to the OS credential store")
    }
    fn delete(&self, url: &str) -> Result<()> {
        match keyring::Entry::new(self.0, url).and_then(|e| e.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e).context("deleting from the OS credential store"),
        }
    }
}

type Saved = BTreeMap<String, BTreeMap<String, String>>;

/// Secrets in one JSON file, keyed by service then account (credentials.ts `fileEntry`). Read on every call, so two
/// processes see each other's writes.
pub struct FileStore {
    file: PathBuf,
    service: String,
}

impl FileStore {
    pub fn new(file: PathBuf, service: &str) -> Self {
        Self {
            file,
            service: service.to_string(),
        }
    }

    fn read(&self) -> Result<Saved> {
        match std::fs::read_to_string(&self.file) {
            Ok(text) => serde_json::from_str(&text).with_context(|| format!("Couldn't read {}", self.file.display())),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(Saved::new()),
            Err(e) => Err(e).with_context(|| format!("Couldn't read {}", self.file.display())),
        }
    }

    /// Written whole to a temp file readable by this user only, then renamed over, so it's never readable by others
    /// nor half written.
    fn write(&self, saved: &Saved) -> Result<()> {
        if let Some(dir) = self.file.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let name = self
            .file
            .file_name()
            .context("the credentials file has no name")?
            .to_string_lossy();
        let tmp = self.file.with_file_name(format!("{name}.{}.tmp", std::process::id()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let file = options
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        #[cfg(unix)]
        std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
        serde_json::to_writer_pretty(file, saved).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.file).with_context(|| format!("writing {}", self.file.display()))
    }
}

impl TokenStore for FileStore {
    fn get(&self, url: &str) -> Result<Option<String>> {
        Ok(self
            .read()?
            .get(&self.service)
            .and_then(|s| s.get(url))
            .filter(|t| !t.is_empty())
            .cloned())
    }
    fn set(&self, url: &str, token: &str) -> Result<()> {
        let mut saved = self.read()?;
        saved
            .entry(self.service.clone())
            .or_default()
            .insert(url.to_string(), token.to_string());
        self.write(&saved)
    }
    fn delete(&self, url: &str) -> Result<()> {
        let mut saved = self.read()?;
        let Some(service) = saved.get_mut(&self.service) else {
            return Ok(());
        };
        if service.remove(url).is_none() {
            return Ok(());
        }
        if service.is_empty() {
            saved.remove(&self.service);
        }
        self.write(&saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_store_keeps_one_token_per_url_under_its_service() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested").join("credentials.json");
        let store = FileStore::new(file.clone(), "retaskable");
        assert_eq!(store.get("https://retaskable.com").unwrap(), None);
        store.set("https://retaskable.com", "r1").unwrap();
        store.set("http://localhost:8787", "r2").unwrap();
        assert_eq!(store.get("https://retaskable.com").unwrap().as_deref(), Some("r1"));
        // Another service in the same file is left alone.
        FileStore::new(file.clone(), "other").set("x", "y").unwrap();
        store.delete("https://retaskable.com").unwrap();
        store.delete("https://retaskable.com").unwrap();
        assert_eq!(store.get("https://retaskable.com").unwrap(), None);
        assert_eq!(store.get("http://localhost:8787").unwrap().as_deref(), Some("r2"));
        let saved: Saved = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(saved["other"]["x"], "y");
        // No temp file is left behind.
        assert_eq!(std::fs::read_dir(file.parent().unwrap()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_store_is_readable_by_this_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credentials.json");
        FileStore::new(file.clone(), "retaskable").set("u", "t").unwrap();
        assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn a_broken_file_says_so_rather_than_reading_as_unlinked() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("credentials.json");
        std::fs::write(&file, "{ not json").unwrap();
        let err = FileStore::new(file, "retaskable").get("u").unwrap_err();
        assert!(format!("{err:#}").contains("Couldn't read"), "{err:#}");
    }

    #[test]
    fn the_override_file_is_used_instead_of_the_os_store() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("scratch.json");
        let path = file.to_string_lossy().to_string();
        let env = |k: &str| (k == "RETASKABLE_CREDENTIALS_FILE").then(|| path.clone());
        keychain(dir.path(), env).set("https://retaskable.com", "t").unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().contains("\"t\""));
        assert!(
            credential_file_note(dir.path(), env)
                .unwrap()
                .contains("RETASKABLE_CREDENTIALS_FILE")
        );
    }
}
