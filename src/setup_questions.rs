//! Setup's questions after `retaskable link` (agent-settings.md section 5; `agent/src/setupQuestions.ts`):
//! each computer-only setting in turn, its current value in brackets, kept on Enter. A setting a
//! variable sets is said, not asked. Answers are checked and saved the way the settings page does
//! (`settings_server::save`).
use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::Result;

use crate::config::{self, ConfigFile};
use crate::env::Env;
use crate::network;
use crate::settings_server::save::{CursorModelsUpdate, LocalUpdate, NetworkUpdate, SaveError, Saves};
use crate::store::MachineStore;
use crate::tags;

/// Reads answers from `input` and writes questions to `out`; `set_echo(false)` hides what is typed.
pub struct Setup<'a> {
    /// Whether someone is at a terminal: without one nothing is asked.
    pub terminal: bool,
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub set_echo: &'a mut dyn FnMut(bool),
}

fn message(e: SaveError) -> String {
    match e {
        SaveError::Invalid(m) => m,
        SaveError::Io(e) => e.to_string(),
    }
}

fn on_off(s: &str) -> Result<bool, String> {
    match s.to_ascii_lowercase().as_str() {
        "on" | "yes" | "y" | "true" => Ok(true),
        "off" | "no" | "n" | "false" => Ok(false),
        _ => Err("Answer on or off.".to_string()),
    }
}

fn word(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

impl Setup<'_> {
    /// One typed line, trimmed; `None` at the end of the input.
    fn line(&mut self, prompt: &str) -> Result<Option<String>> {
        write!(self.out, "{prompt}")?;
        self.out.flush()?;
        let mut s = String::new();
        if self.input.read_line(&mut s)? == 0 {
            writeln!(self.out)?;
            return Ok(None);
        }
        Ok(Some(s.trim().to_string()))
    }

    /// Asks until `check` accepts the answer (its refusal is shown); `None` keeps the current value
    /// (Enter, or no more input).
    fn ask<T>(&mut self, prompt: &str, check: impl Fn(&str) -> Result<T, String>) -> Result<Option<T>> {
        loop {
            match self.line(prompt)? {
                None => return Ok(None),
                Some(s) if s.is_empty() => return Ok(None),
                Some(s) => match check(&s) {
                    Ok(v) => return Ok(Some(v)),
                    Err(m) => writeln!(self.out, "{m}")?,
                },
            }
        }
    }

    /// Whether `var` sets this setting; if so it says so instead of asking.
    fn set_by(&mut self, name: &str, vars: &[&str], env: &Env) -> Result<bool> {
        match vars.iter().find(|v| env.get(v).is_some()) {
            Some(v) => {
                writeln!(self.out, "{name}: set by {v}, change it there.")?;
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Asks every setting, saving the changed ones, then says to restart.
    pub fn run(&mut self, dir: &Path, cfg: &ConfigFile, env: &Env, default_port: u16) -> Result<()> {
        if !self.terminal {
            return Ok(());
        }
        let saves = Saves { dir, env };
        self.port(&saves, cfg, default_port)?;
        self.local(&saves, cfg)?;
        self.poll(&saves, cfg)?;
        self.tags(&saves, dir)?;
        self.cursor(&saves, cfg)?;
        self.network(&saves)?;
        writeln!(self.out, "Saved. If Retaskable is running, restart it to use them.")?;
        Ok(())
    }

    fn port(&mut self, saves: &Saves, cfg: &ConfigFile, default_port: u16) -> Result<()> {
        if self.set_by("Port", &["RETASKABLE_PORT"], saves.env)? {
            return Ok(());
        }
        let now = config::port(cfg, saves.env, default_port);
        let typed = self.ask(&format!("Port [{now}]: "), |s| {
            let port = s
                .parse::<u16>()
                .map_err(|_| "Port must be 1024 to 65535.".to_string())?;
            saves.validate_port(port).map_err(message)?;
            Ok(port)
        })?;
        if let Some(port) = typed.filter(|&p| p != now) {
            saves.save_port(port).map_err(into_anyhow)?;
        }
        Ok(())
    }

    fn local(&mut self, saves: &Saves, cfg: &ConfigFile) -> Result<()> {
        if self.set_by(
            "Local model",
            &["RETASKABLE_LOCAL_MODEL", "RETASKABLE_LOCAL_URL"],
            saves.env,
        )? {
            return Ok(());
        }
        let now = config::local_model(cfg, saves.env);
        let enabled = self
            .ask(&format!("Local model on or off [{}]: ", word(now.enabled)), on_off)?
            .unwrap_or(now.enabled);
        let mut update = LocalUpdate {
            enabled,
            model: now.model.clone(),
            base_url: now.base_url.clone(),
        };
        if enabled {
            let shown = if now.model.is_empty() { "none" } else { &now.model };
            let model = self.ask(&format!("Local model's name [{shown}]: "), |s| {
                let candidate = LocalUpdate {
                    model: s.to_string(),
                    ..update.clone()
                };
                saves.validate_local(&candidate).map_err(message)?;
                Ok(s.to_string())
            })?;
            update.model = model.unwrap_or(now.model.clone());
            let url = self.ask(&format!("Local model's address [{}]: ", now.base_url), |s| {
                let candidate = LocalUpdate {
                    base_url: s.to_string(),
                    ..update.clone()
                };
                saves.validate_local(&candidate).map_err(message)?;
                Ok(s.to_string())
            })?;
            update.base_url = url.unwrap_or(now.base_url.clone());
        }
        if enabled != now.enabled || (enabled && (update.model != now.model || update.base_url != now.base_url)) {
            saves.save_local(&update).map_err(into_anyhow)?;
        }
        Ok(())
    }

    fn poll(&mut self, saves: &Saves, cfg: &ConfigFile) -> Result<()> {
        if self.set_by("How often to check for work", &["RETASKABLE_POLL_SEC"], saves.env)? {
            return Ok(());
        }
        let now = config::poll_interval_sec(cfg, saves.env);
        let typed = self.ask(&format!("Seconds between checks for work [{now}]: "), |s| {
            let secs = s
                .parse::<u32>()
                .map_err(|_| "How often it checks must be 2 to 3600 seconds.".to_string())?;
            saves.validate_poll_interval(secs).map_err(message)?;
            Ok(secs)
        })?;
        if let Some(secs) = typed.filter(|&n| n != now) {
            saves.save_poll_interval(secs).map_err(into_anyhow)?;
        }
        Ok(())
    }

    fn tags(&mut self, saves: &Saves, dir: &Path) -> Result<()> {
        if self.set_by("Tags", &["RETASKABLE_TAGS"], saves.env)? {
            return Ok(());
        }
        let found = tags::found_tags(&tags::machine_facts(saves.env));
        let now = tags::computer_tags(&found, MachineStore::load(dir)?.tags().as_ref());
        let shown = if now.is_empty() {
            "none".to_string()
        } else {
            now.join(", ")
        };
        let typed = self.ask(&format!("Tags, comma between, or none [{shown}]: "), |s| {
            Ok(if s.eq_ignore_ascii_case("none") {
                Vec::new()
            } else {
                s.split(',').map(str::to_string).collect::<Vec<_>>()
            })
        })?;
        if let Some(list) = typed.filter(|l| tags::clean_tags(l.to_vec().into()) != now) {
            saves.save_tags(&list)?;
        }
        Ok(())
    }

    fn cursor(&mut self, saves: &Saves, cfg: &ConfigFile) -> Result<()> {
        if self.set_by("Cursor's models", &["RETASKABLE_CURSOR_MODELS"], saves.env)? {
            return Ok(());
        }
        let now = config::cursor_models(cfg, saves.env);
        let mut next = [now.top.clone(), now.implementer.clone(), now.scout.clone()];
        let tiers = ["planning and review", "building", "scouts and small jobs"];
        for (slot, tier) in next.iter_mut().zip(tiers) {
            let shown = slot.clone().unwrap_or_else(|| "auto".to_string());
            let typed = self.ask(&format!("Cursor's model for {tier}, or auto [{shown}]: "), |s| {
                let one = (!s.eq_ignore_ascii_case("auto")).then(|| s.to_string());
                let probe = CursorModelsUpdate {
                    top: one.clone(),
                    ..Default::default()
                };
                saves.validate_cursor_models(&probe).map_err(message)?;
                Ok(one)
            })?;
            if let Some(model) = typed {
                *slot = model;
            }
        }
        let [top, implementer, scout] = next;
        if (&top, &implementer, &scout) != (&now.top, &now.implementer, &now.scout) {
            saves
                .save_cursor_models(&CursorModelsUpdate {
                    top,
                    implementer,
                    scout,
                })
                .map_err(into_anyhow)?;
        }
        Ok(())
    }

    fn network(&mut self, saves: &Saves) -> Result<()> {
        if self.set_by("Network access", &["RETASKABLE_NETWORK_PASSWORD"], saves.env)? {
            return Ok(());
        }
        let stored = MachineStore::load(saves.dir)?.network();
        let was_on = stored.as_ref().is_some_and(|n| n.enabled);
        let has_hash = stored.as_ref().is_some_and(|n| n.password_hash.is_some());
        let on = self
            .ask(&format!("Network access on or off [{}]: ", word(was_on)), on_off)?
            .unwrap_or(was_on);
        if !on {
            if was_on {
                saves
                    .save_network(&NetworkUpdate {
                        enabled: false,
                        password: None,
                    })
                    .map_err(into_anyhow)?;
            }
            return Ok(());
        }
        let hint = if has_hash {
            "Enter keeps the saved one"
        } else {
            "at least 8 characters"
        };
        (self.set_echo)(false);
        let typed = self.ask(&format!("Password ({hint}): "), |s| {
            if s.encode_utf16().count() < network::MIN_PASSWORD_LEN {
                return Err(format!(
                    "Password must be at least {} characters.",
                    network::MIN_PASSWORD_LEN
                ));
            }
            Ok(s.to_string())
        });
        (self.set_echo)(true);
        writeln!(self.out)?;
        match typed? {
            Some(password) => saves
                .save_network(&NetworkUpdate {
                    enabled: true,
                    password: Some(password),
                })
                .map_err(into_anyhow)?,
            None if has_hash => {
                if !was_on {
                    saves
                        .save_network(&NetworkUpdate {
                            enabled: true,
                            password: None,
                        })
                        .map_err(into_anyhow)?;
                }
            }
            None => {
                writeln!(self.out, "Network access needs a password, so it stays off.")?;
                if was_on {
                    saves
                        .save_network(&NetworkUpdate {
                            enabled: false,
                            password: None,
                        })
                        .map_err(into_anyhow)?;
                }
            }
        }
        Ok(())
    }
}

fn into_anyhow(e: SaveError) -> anyhow::Error {
    anyhow::anyhow!(message(e))
}

/// Turns the terminal's echo on or off, so a password isn't shown as it is typed.
pub fn terminal_echo(on: bool) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("stty")
            .arg(if on { "echo" } else { "-echo" })
            .status();
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
        };
        // SAFETY: plain Win32 console calls on the process's own stdin handle.
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0;
            if GetConsoleMode(h, &mut mode) != 0 {
                let mode = if on {
                    mode | ENABLE_ECHO_INPUT
                } else {
                    mode & !ENABLE_ECHO_INPUT
                };
                SetConsoleMode(h, mode);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Runs setup with `typed` as the answers; returns what it printed.
    fn run(dir: &Path, env: &Env, typed: &str) -> String {
        let cfg = config::load(dir).unwrap();
        let mut input = typed.as_bytes();
        let mut out = Vec::new();
        let mut echo = |_: bool| {};
        Setup {
            terminal: true,
            input: &mut input,
            out: &mut out,
            set_echo: &mut echo,
        }
        .run(dir, &cfg, env, 4700)
        .unwrap();
        String::from_utf8(out).unwrap()
    }

    fn file(dir: &Path, name: &str) -> Value {
        std::fs::read_to_string(dir.join(name))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null)
    }

    #[test]
    fn typed_answers_are_saved_to_config_and_machine() {
        let dir = tempfile::tempdir().unwrap();
        let typed = "5000\non\nqwen3\nhttp://127.0.0.1:1234\n30\ngames, web\ngpt-5\n\nauto\non\nlongenough\n";
        let out = run(dir.path(), &Env::empty(), typed);
        assert!(out.contains("Port [4700]: "));
        assert!(out.ends_with("Saved. If Retaskable is running, restart it to use them.\n"));
        let cfg = file(dir.path(), "config.json");
        assert_eq!(cfg["port"], 5000);
        assert_eq!(cfg["pollIntervalSec"], 30);
        assert_eq!(cfg["local"]["enabled"], true);
        assert_eq!(cfg["local"]["model"], "qwen3");
        assert_eq!(cfg["local"]["baseUrl"], "http://127.0.0.1:1234");
        assert_eq!(cfg["cursor"]["models"]["top"], "gpt-5");
        assert!(cfg["cursor"]["models"].get("implementer").is_none());
        let machine = file(dir.path(), "machine.json");
        assert!(
            machine["tags"]["added"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t == "games")
        );
        assert_eq!(machine["network"]["enabled"], true);
        let hash = machine["network"]["passwordHash"].as_str().unwrap();
        assert!(hash.starts_with("scrypt$"));
        assert!(crate::password::verify_password("longenough", hash));
    }

    #[test]
    fn enter_on_every_question_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &Env::empty(), "\n\n\n\n\n\n\n\n");
        assert!(!dir.path().join("config.json").exists());
        assert!(!dir.path().join("machine.json").exists());
    }

    #[test]
    fn a_setting_a_variable_sets_is_said_not_asked() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::from_pairs([("RETASKABLE_PORT", "4800"), ("RETASKABLE_POLL_SEC", "9")]);
        let out = run(dir.path(), &env, "\n\n\n\n\n\n");
        assert!(out.contains("Port: set by RETASKABLE_PORT, change it there."));
        assert!(out.contains("How often to check for work: set by RETASKABLE_POLL_SEC"));
        assert!(!out.contains("Port ["));
    }

    #[test]
    fn an_invalid_port_is_asked_again_in_the_pages_words() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(dir.path(), &Env::empty(), "80\nabc\n5001\n");
        assert_eq!(out.matches("Port must be 1024 to 65535.").count(), 2);
        assert_eq!(file(dir.path(), "config.json")["port"], 5001);
    }

    #[test]
    fn network_access_with_a_short_password_asks_again() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(dir.path(), &Env::empty(), "\n\n\n\n\n\n\non\nshort\nlongenough\n");
        assert!(out.contains("Password must be at least 8 characters."));
        assert_eq!(file(dir.path(), "machine.json")["network"]["enabled"], true);
    }

    #[test]
    fn network_access_on_with_no_password_stays_off() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(dir.path(), &Env::empty(), "\n\n\n\n\n\n\non\n\n");
        assert!(out.contains("stays off"));
        assert!(file(dir.path(), "machine.json")["network"]["enabled"] != true);
    }

    #[test]
    fn without_a_terminal_nothing_is_asked() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config::load(dir.path()).unwrap();
        let mut input = "5000\n".as_bytes();
        let mut out = Vec::new();
        let mut echo = |_: bool| {};
        Setup {
            terminal: false,
            input: &mut input,
            out: &mut out,
            set_echo: &mut echo,
        }
        .run(dir.path(), &cfg, &Env::empty(), 4700)
        .unwrap();
        assert!(out.is_empty());
        assert!(!dir.path().join("config.json").exists());
    }

    #[test]
    fn the_password_is_not_echoed() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config::load(dir.path()).unwrap();
        let mut input = "\n\n\n\n\n\n\non\nlongenough\n".as_bytes();
        let mut out = Vec::new();
        let mut calls = Vec::new();
        let mut echo = |on: bool| calls.push(on);
        Setup {
            terminal: true,
            input: &mut input,
            out: &mut out,
            set_echo: &mut echo,
        }
        .run(dir.path(), &cfg, &Env::empty(), 4700)
        .unwrap();
        assert_eq!(calls, vec![false, true]);
    }
}
