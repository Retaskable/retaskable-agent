//! Where the data key comes from (agent-settings.md section 3.3), decided once at start:
//!
//! 1. `RETASKABLE_SECRETS_KEY_FILE`: a file holding the key as base64. On Unix it must be readable by this user only
//!    (0600); one that isn't is refused. If the file isn't there yet, the first save creates it with a fresh key.
//! 2. `RETASKABLE_SECRETS_KEY`: the key as base64, for a container whose operator keeps it in the compose file's
//!    environment or a secret.
//! 3. The OS credential store (Windows Credential Manager, macOS Keychain, the Secret Service), service
//!    `retaskable-secrets-key`, account a stable id of the data folder. The key is made there on the first save.
//! 4. None of these: secrets can't be saved, and the page says why.
//!
//! A key that can't be read means every saved secret reads as unreadable; nothing falls back to plaintext.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

use super::cipher::DataKey;
use crate::env::Env;
use crate::link::TokenStore;
use crate::sync::lock;
use crate::text::is_blank;

/// The OS credential store's service for the data key.
pub const KEY_SERVICE: &str = "retaskable-secrets-key";
pub const KEY_FILE_VAR: &str = "RETASKABLE_SECRETS_KEY_FILE";
pub const KEY_VAR: &str = "RETASKABLE_SECRETS_KEY";

/// The OS credential store account for the data folder `dir`: a hash of its absolute path, so two data folders on one
/// computer (a release build and a checkout) each have their own key, and no path is written into the store.
pub fn account_for(dir: &Path) -> String {
    let path = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let digest = Sha256::digest(path.to_string_lossy().as_bytes());
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

enum Source {
    /// The key itself, from the environment or a key file that was there.
    Given(DataKey),
    /// A key file named but not there yet: made on the first save.
    FileToMake(PathBuf),
    Os {
        store: Arc<dyn TokenStore>,
        account: String,
    },
    /// Named but unusable; the message says how, with no key in it.
    Broken(String),
    NoKey,
}

pub struct Keys {
    source: Source,
    /// The OS key once read or made, so the store is asked once.
    cached: Mutex<Option<DataKey>>,
}

impl Keys {
    pub fn open(dir: &Path, env: &Env, os: Option<Arc<dyn TokenStore>>) -> Self {
        Self {
            source: Self::source(dir, env, os),
            cached: Mutex::new(None),
        }
    }

    fn source(dir: &Path, env: &Env, os: Option<Arc<dyn TokenStore>>) -> Source {
        if let Some(file) = env.get(KEY_FILE_VAR).filter(|f| !is_blank(f)) {
            return from_file(Path::new(file.trim()));
        }
        if let Some(text) = env.get(KEY_VAR).filter(|v| !is_blank(v)) {
            return match DataKey::from_base64(&text) {
                Some(key) => Source::Given(key),
                None => Source::Broken(format!("{KEY_VAR} must be a 32-byte key as base64.")),
            };
        }
        match os {
            Some(store) => Source::Os {
                store,
                account: account_for(dir),
            },
            None => Source::NoKey,
        }
    }

    /// The key to read with, or `None` when none exists yet (nothing was ever saved under it). `Err` says why it can't
    /// be had.
    pub fn for_reading(&self) -> Result<Option<DataKey>, String> {
        match &self.source {
            Source::Given(key) => Ok(Some(key.clone())),
            Source::FileToMake(_) | Source::NoKey => Ok(None),
            Source::Broken(why) => Err(why.clone()),
            Source::Os { store, account } => {
                let mut cached = lock(&self.cached);
                if cached.is_none() {
                    let stored = store.get(account).map_err(|_| {
                        "The OS credential store wouldn't give up the key for saved secrets.".to_string()
                    })?;
                    *cached = match stored {
                        None => None,
                        Some(text) => Some(DataKey::from_base64(&text).ok_or_else(|| {
                            "The key for saved secrets in the OS credential store isn't a 32-byte key.".to_string()
                        })?),
                    };
                }
                Ok(cached.clone())
            }
        }
    }

    /// The key to save with, made first when there is none and one can be.
    pub fn for_saving(&self) -> Result<DataKey, String> {
        if let Some(key) = self.for_reading()? {
            return Ok(key);
        }
        match &self.source {
            Source::FileToMake(file) => {
                let key = DataKey::generate();
                write_key_file(file, &key).map_err(|_| format!("Couldn't write the key file {}.", file.display()))?;
                Ok(key)
            }
            Source::Os { store, account } => {
                let key = DataKey::generate();
                store
                    .set(account, &key.to_base64())
                    .map_err(|_| "Couldn't save a key for secrets in the OS credential store.".to_string())?;
                *lock(&self.cached) = Some(key.clone());
                Ok(key)
            }
            Source::NoKey => Err(no_key_note()),
            // `for_reading` already answered these.
            Source::Given(key) => Ok(key.clone()),
            Source::Broken(why) => Err(why.clone()),
        }
    }

    /// Why a save can't work on this computer, or `None`. Doesn't touch the OS credential store.
    pub fn save_problem(&self) -> Option<String> {
        match &self.source {
            Source::Broken(why) => Some(why.clone()),
            Source::NoKey => Some(no_key_note()),
            _ => None,
        }
    }
}

fn no_key_note() -> String {
    format!(
        "This computer has no OS credential store to keep the key for saved secrets, so they can't be saved here. Set \
         {KEY_FILE_VAR} (a file holding a 32-byte key as base64, readable by this user only) or {KEY_VAR}, then restart."
    )
}

fn from_file(file: &Path) -> Source {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Source::FileToMake(file.to_path_buf()),
        Err(_) => return Source::Broken(format!("Couldn't read the key file {}.", file.display())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(file).map(|m| m.permissions().mode()).unwrap_or(0o777);
        if mode & 0o077 != 0 {
            return Source::Broken(format!(
                "The key file {} must be readable by this user only (chmod 600).",
                file.display()
            ));
        }
    }
    match DataKey::from_base64(&text) {
        Some(key) => Source::Given(key),
        None => Source::Broken(format!(
            "The key file {} must hold a 32-byte key as base64.",
            file.display()
        )),
    }
}

/// Created 0600 where the platform has modes, never wider even for a moment.
fn write_key_file(file: &Path, key: &DataKey) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut out = options.open(file)?;
    out.write_all(key.to_base64().as_bytes())
}
