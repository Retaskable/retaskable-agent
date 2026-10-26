//! Where the agent keeps its files and which port it listens on, read the same way as the TypeScript agent
//! (Retaskable.com agent/src/config.ts and paths.ts): `RETASKABLE_PORT`, then `port` in `config.json`, then 4700.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

pub const DEFAULT_PORT: u16 = 4700;

/// The settings this agent reads so far. Every other key in `config.json` is kept for later and ignored.
#[derive(Debug, Default, Deserialize)]
pub struct ConfigFile {
    pub port: Option<u16>,
}

/// `RETASKABLE_DATA_DIR`, or the working folder's `.retaskable/`, as a checkout of the TypeScript agent uses.
pub fn data_dir(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    env("RETASKABLE_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".retaskable"))
}

/// The config file, or the defaults when there's none yet.
pub fn load(dir: &Path) -> Result<ConfigFile> {
    let file = dir.join("config.json");
    match std::fs::read_to_string(&file) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("reading {}", file.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ConfigFile::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
    }
}

// agent-settings.md section 3.1: "Port: a whole number from 1024 to 65535." Anything below
// that (0 included) is as invalid as an unparseable value and falls through the same way.
fn in_range(p: u16) -> bool {
    p >= 1024
}

pub fn port(cfg: &ConfigFile, env: impl Fn(&str) -> Option<String>) -> u16 {
    env("RETASKABLE_PORT")
        .and_then(|p| p.parse().ok())
        .filter(|&p| in_range(p))
        .or(cfg.port.filter(|&p| in_range(p)))
        .unwrap_or(DEFAULT_PORT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn port_prefers_the_env_then_the_file_then_4700() {
        let env = |k: &str| (k == "RETASKABLE_PORT").then(|| "4800".to_string());
        assert_eq!(port(&ConfigFile { port: Some(4900) }, env), 4800);
        assert_eq!(port(&ConfigFile { port: Some(4900) }, no_env), 4900);
        assert_eq!(port(&ConfigFile::default(), no_env), DEFAULT_PORT);
        let bad = |k: &str| (k == "RETASKABLE_PORT").then(|| "nope".to_string());
        assert_eq!(port(&ConfigFile::default(), bad), DEFAULT_PORT);
        // An unparseable env value falls through to the file, not straight to 4700.
        assert_eq!(port(&ConfigFile { port: Some(4900) }, bad), 4900);
    }

    #[test]
    fn a_zero_env_port_is_not_used_either() {
        let zero = |k: &str| (k == "RETASKABLE_PORT").then(|| "0".to_string());
        assert_eq!(port(&ConfigFile { port: Some(4900) }, zero), 4900);
        assert_eq!(port(&ConfigFile::default(), zero), DEFAULT_PORT);
    }

    #[test]
    fn a_zero_port_in_the_file_is_not_used_either() {
        // Binding port 0 asks the OS for a random ephemeral port, not the documented
        // fallback, so a zero in config.json is as invalid as a zero env value.
        assert_eq!(port(&ConfigFile { port: Some(0) }, no_env), DEFAULT_PORT);
    }

    #[test]
    fn a_privileged_port_below_1024_is_not_used_either() {
        // agent-settings.md section 3.1: the documented valid range is 1024 to 65535,
        // so a sub-1024 port (env or file) is as invalid as zero and falls through too.
        let low_env = |k: &str| (k == "RETASKABLE_PORT").then(|| "80".to_string());
        assert_eq!(port(&ConfigFile { port: Some(4900) }, low_env), 4900);
        assert_eq!(port(&ConfigFile::default(), low_env), DEFAULT_PORT);
        assert_eq!(port(&ConfigFile { port: Some(80) }, no_env), DEFAULT_PORT);
        assert_eq!(port(&ConfigFile { port: Some(1024) }, no_env), 1024);
    }

    #[test]
    fn a_missing_file_is_the_defaults_and_other_keys_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load(dir.path()).unwrap().port, None);
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"version":2,"port":4711,"projects":[]}"#,
        )
        .unwrap();
        assert_eq!(load(dir.path()).unwrap().port, Some(4711));
    }

    #[test]
    fn a_broken_file_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.json"), "{").unwrap();
        assert!(format!("{:#}", load(dir.path()).unwrap_err()).contains("config.json"));
    }

    #[test]
    fn the_data_dir_comes_from_the_env_or_the_working_folder() {
        assert_eq!(data_dir(no_env), PathBuf::from(".retaskable"));
        let env = |k: &str| (k == "RETASKABLE_DATA_DIR").then(|| "x".to_string());
        assert_eq!(data_dir(env), PathBuf::from("x"));
    }
}
