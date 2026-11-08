//! A release build opens the settings page once by itself, on its first run with no config file yet
//! (agent-settings.md section 4.4), and remembers it as `settingsOpened` in machine.json.
use anyhow::Result;

use super::SettingsServer;
use crate::store::MachineStore;

impl SettingsServer {
    /// Opens the settings page through `open` when this is a release build with no `config.json` and
    /// `settingsOpened` unset, then sets `settingsOpened`. Returns whether it opened. A checkout build
    /// never does. It's remembered even when `open` fails, so a computer with no browser isn't tried
    /// on every start.
    pub fn open_on_first_run(&self, is_release: bool, open: impl FnOnce(&str) -> Result<()>) -> Result<bool> {
        if !is_release || self.dir.join("config.json").exists() {
            return Ok(false);
        }
        let mut machine = MachineStore::load(&self.dir)?;
        if machine.settings_opened() {
            return Ok(false);
        }
        if let Err(e) = open(&self.settings_link()) {
            tracing::warn!("couldn't open the settings page: {e:#}");
        }
        machine.set_settings_opened()?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::settings_with_network;
    use crate::store::MachineStore;
    use std::cell::RefCell;

    fn run(server: &super::SettingsServer, release: bool) -> (bool, Vec<String>) {
        let opened = RefCell::new(vec![]);
        let did = server
            .open_on_first_run(release, |url| {
                opened.borrow_mut().push(url.to_string());
                Ok(())
            })
            .unwrap();
        (did, opened.into_inner())
    }

    #[test]
    fn opens_once_on_a_release_first_run() {
        let dir = tempfile::tempdir().unwrap();
        let server = settings_with_network(dir.path(), false, None);
        let (did, urls) = run(&server, true);
        assert!(did);
        assert_eq!(urls.len(), 1);
        assert!(urls[0].starts_with("http://127.0.0.1:4700/settings#"));
        assert!(MachineStore::load(dir.path()).unwrap().settings_opened());
        assert_eq!(run(&server, true), (false, vec![]));
    }

    #[test]
    fn not_with_a_config_file_or_in_a_checkout_build() {
        let dir = tempfile::tempdir().unwrap();
        let server = settings_with_network(dir.path(), false, None);
        assert_eq!(run(&server, false), (false, vec![]));
        assert!(!MachineStore::load(dir.path()).unwrap().settings_opened());
        std::fs::write(dir.path().join("config.json"), "{}").unwrap();
        assert_eq!(run(&server, true), (false, vec![]));
        assert!(!MachineStore::load(dir.path()).unwrap().settings_opened());
    }
}
