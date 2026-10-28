//! Network access to the settings page (agent-settings.md sections 3.1, 3.2, 4.4): off by
//! default. On needs a password of at least 8 characters, kept as its scrypt hash in
//! `machine.json`'s `network` key; turning it off keeps the hash, so turning it on again needs no
//! new one. Changes to the file take effect only once the agent restarts, the same as the port.
use serde_json::{Map, Value};

use crate::password::hash_password;

/// "On needs a password of at least 8 characters."
pub const MIN_PASSWORD_LEN: usize = 8;

/// `machine.json`'s `network` key.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NetworkAccess {
    pub enabled: bool,
    pub password_hash: Option<String>,
}

impl NetworkAccess {
    /// `enabled` with no `passwordHash` reads as off: the page never lets that combination be
    /// saved, so it only happens to a hand-edited file, and access needs a password regardless.
    pub fn from_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let password_hash = obj.get("passwordHash").and_then(Value::as_str).map(str::to_string);
        let enabled = obj.get("enabled").and_then(Value::as_bool).unwrap_or(false) && password_hash.is_some();
        Some(Self { enabled, password_hash })
    }

    pub fn to_value(&self) -> Value {
        let mut obj = Map::new();
        obj.insert("enabled".to_string(), Value::Bool(self.enabled));
        if let Some(hash) = &self.password_hash {
            obj.insert("passwordHash".to_string(), Value::String(hash.clone()));
        }
        Value::Object(obj)
    }
}

/// `RETASKABLE_NETWORK_PASSWORD`: network access on, with this password, hashed fresh on every
/// start (never written to the file, the same way an environment value never is). Fewer than
/// `MIN_PASSWORD_LEN` characters doesn't read, so it falls through to the file's own value, the
/// same as any other setting's variable outside its documented range.
pub fn network_access(stored: Option<&NetworkAccess>, env: impl Fn(&str) -> Option<String>) -> NetworkAccess {
    if let Some(password) = env("RETASKABLE_NETWORK_PASSWORD")
        && password.encode_utf16().count() >= MIN_PASSWORD_LEN
    {
        return NetworkAccess {
            enabled: true,
            password_hash: Some(hash_password(&password)),
        };
    }
    stored.cloned().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn off_by_default() {
        assert_eq!(network_access(None, no_env), NetworkAccess::default());
    }

    #[test]
    fn the_env_password_turns_it_on_with_a_fresh_hash() {
        let env = |k: &str| (k == "RETASKABLE_NETWORK_PASSWORD").then(|| "a long enough password".to_string());
        let result = network_access(None, env);
        assert!(result.enabled);
        assert!(result.password_hash.unwrap().starts_with("scrypt$"));
    }

    #[test]
    fn a_too_short_env_password_falls_through_to_the_file() {
        let stored = NetworkAccess {
            enabled: true,
            password_hash: Some("scrypt$16384$8$1$aa$bb".to_string()),
        };
        let env = |k: &str| (k == "RETASKABLE_NETWORK_PASSWORD").then(|| "short".to_string());
        assert_eq!(network_access(Some(&stored), env), stored);
    }

    #[test]
    fn turning_it_off_keeps_the_hash_in_the_file_and_round_trips() {
        let stored = NetworkAccess {
            enabled: false,
            password_hash: Some("scrypt$16384$8$1$aa$bb".to_string()),
        };
        let value = stored.to_value();
        assert_eq!(NetworkAccess::from_value(&value).unwrap(), stored);
    }

    #[test]
    fn enabled_with_no_password_hash_reads_as_off() {
        let value = serde_json::json!({ "enabled": true });
        assert_eq!(
            NetworkAccess::from_value(&value).unwrap(),
            NetworkAccess {
                enabled: false,
                password_hash: None
            }
        );
    }
}
