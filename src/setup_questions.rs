//! The computer-only settings `retaskable link` asks once it has linked (agent-settings.md
//! section 5): each in turn with what it is now in brackets, which Enter keeps. A setting a
//! variable sets is said, not asked. Answers are saved as the settings page saves them
//! (`settings_server::save::Saver`). Without a terminal nothing is asked.
use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::Result;

use crate::config;
use crate::env::Env;
use crate::network;
use crate::settings_server::AGENT_ENV_VARS;
use crate::settings_server::save::{CursorModelsUpdate, LocalUpdate, NetworkUpdate, SaveError, Saver};
use crate::store::MachineStore;
use crate::tags;

/// Where the questions are read from and written to. `secret` reads a line from `input` with
/// no echo.
pub struct Terminal<'a> {
    pub is_terminal: bool,
    pub input: &'a mut dyn BufRead,
    pub out: &'a mut dyn Write,
    pub secret: &'a mut dyn FnMut(&mut dyn BufRead) -> io::Result<String>,
}

impl Terminal<'_> {
    /// The typed line, trimmed; `None` at the end of input.
    fn line(&mut self, prompt: &str) -> Result<Option<String>> {
        write!(self.out, "{prompt}")?;
        self.out.flush()?;
        let mut buf = String::new();
        if self.input.read_line(&mut buf)? == 0 {
            return Ok(None);
        }
        Ok(Some(buf.trim().to_string()))
    }

    /// Asks until `apply` takes the answer (`true`), a refusal is printed in the page's words
    /// and asked again, and Enter (or the end of input) keeps the setting (`false`). With
    /// `keep_on_enter` false, Enter goes to `apply` too.
    fn ask(
        &mut self,
        label: &str,
        current: &str,
        keep_on_enter: bool,
        mut apply: impl FnMut(&str) -> Result<(), SaveError>,
    ) -> Result<bool> {
        loop {
            let Some(answer) = self.line(&format!("{label} [{current}]: "))? else {
                return Ok(false);
            };
            if answer.is_empty() && keep_on_enter {
                return Ok(false);
            }
            match apply(&answer) {
                Ok(()) => return Ok(true),
                Err(SaveError::Invalid(message)) => writeln!(self.out, "{message}")?,
                Err(SaveError::Io(e)) => return Err(e),
            }
        }
    }

    /// An on/off question; `Some(true)` for on, `Some(false)` for off, `None` when kept.
    fn ask_on_off(&mut self, label: &str, current: bool) -> Result<Option<bool>> {
        let mut answer = None;
        self.ask(label, on_off(current), true, |typed| {
            answer = Some(parse_on_off(typed).ok_or_else(|| SaveError::Invalid("Answer on or off.".to_string()))?);
            Ok(())
        })?;
        Ok(answer)
    }

    fn said(&mut self, label: &str, var: &str) -> Result<()> {
        writeln!(self.out, "{label}: set by {var}, change it there.")?;
        Ok(())
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn parse_on_off(typed: &str) -> Option<bool> {
    match typed.to_ascii_lowercase().as_str() {
        "on" | "yes" | "y" | "true" | "1" => Some(true),
        "off" | "no" | "n" | "false" | "0" => Some(false),
        _ => None,
    }
}

fn is_set(env: &Env, var: &str) -> bool {
    debug_assert!(AGENT_ENV_VARS.contains(&var));
    env.get(var).is_some()
}

/// Asks the questions and saves the answers. `default_port` is this build's own (the port shown
/// when none is set). `flag_on` is the `machine-settings` release flag: Off asks nothing, as
/// before.
pub fn run(dir: &Path, env: &Env, default_port: u16, flag_on: bool, term: &mut Terminal) -> Result<()> {
    if !flag_on || !term.is_terminal {
        return Ok(());
    }
    let saver = Saver { dir, env };
    let cfg = config::load(dir)?;
    let mut saved = false;

    // Port.
    if is_set(env, "RETASKABLE_PORT") {
        term.said("Port", "RETASKABLE_PORT")?;
    } else {
        let current = config::port(&cfg, env, default_port);
        let mut typed = None;
        term.ask("Port", &current.to_string(), true, |t| {
            let port = t
                .parse::<u16>()
                .map_err(|_| SaveError::Invalid("Port must be 1024 to 65535.".to_string()))?;
            saver.validate_port(port)?;
            typed = Some(port);
            Ok(())
        })?;
        if let Some(port) = typed {
            saver.save_port(port)?;
            saved = true;
        }
    }

    // Local model: on or off, then its model and address.
    if is_set(env, "RETASKABLE_LOCAL_MODEL") || is_set(env, "RETASKABLE_LOCAL_URL") {
        let var = if is_set(env, "RETASKABLE_LOCAL_MODEL") {
            "RETASKABLE_LOCAL_MODEL"
        } else {
            "RETASKABLE_LOCAL_URL"
        };
        term.said("Local model", var)?;
    } else {
        let current = cfg.machine.local.clone();
        let mut update = LocalUpdate {
            enabled: current.enabled,
            model: current.model.clone(),
            base_url: current.base_url.clone(),
        };
        let mut changed = false;
        if let Some(on) = term.ask_on_off("Local model on or off", current.enabled)? {
            changed |= on != current.enabled;
            update.enabled = on;
        }
        if update.enabled {
            let shown = current.model.clone();
            let mut model = update.model.clone();
            changed |= term.ask("Local model's name", &shown, !shown.is_empty(), |t| {
                saver.validate_local(&LocalUpdate {
                    enabled: true,
                    model: t.to_string(),
                    base_url: current.base_url.clone(),
                })?;
                model = t.to_string();
                Ok(())
            })?;
            update.model = model;
            let mut url = update.base_url.clone();
            changed |= term.ask("Local model's address", &current.base_url, true, |t| {
                saver.validate_local(&LocalUpdate {
                    enabled: true,
                    model: update.model.clone(),
                    base_url: t.to_string(),
                })?;
                url = t.to_string();
                Ok(())
            })?;
            update.base_url = url;
        }
        if changed {
            match saver.save_local(&update) {
                Ok(()) => saved = true,
                Err(SaveError::Invalid(message)) => writeln!(term.out, "{message}")?,
                Err(SaveError::Io(e)) => return Err(e),
            }
        }
    }

    // How often it checks.
    if is_set(env, "RETASKABLE_POLL_SEC") {
        term.said("How often to check for work, in seconds", "RETASKABLE_POLL_SEC")?;
    } else {
        let current = config::poll_interval_sec(&cfg, env);
        let mut typed = None;
        term.ask(
            "How often to check for work, in seconds",
            &current.to_string(),
            true,
            |t| {
                let secs = t
                    .parse::<u32>()
                    .map_err(|_| SaveError::Invalid("How often it checks must be 2 to 3600 seconds.".to_string()))?;
                saver.validate_poll_interval(secs)?;
                typed = Some(secs);
                Ok(())
            },
        )?;
        if let Some(secs) = typed {
            saver.save_poll_interval(secs)?;
            saved = true;
        }
    }

    // Tags.
    let machine = MachineStore::load(dir)?;
    if is_set(env, "RETASKABLE_TAGS") {
        term.said("Tags", "RETASKABLE_TAGS")?;
    } else {
        let found = tags::found_tags(&tags::machine_facts(env));
        let effective = tags::effective_tags(&found, machine.tags().as_ref(), env);
        let shown = if effective.is_empty() {
            "none".to_string()
        } else {
            effective.join(", ")
        };
        let mut typed = None;
        term.ask("Tags, comma between (none for none)", &shown, true, |t| {
            typed = Some(if t.eq_ignore_ascii_case("none") {
                Vec::new()
            } else {
                t.split(',').map(|s| s.trim().to_string()).collect()
            });
            Ok(())
        })?;
        if let Some(list) = typed {
            saver.save_tags(&list)?;
            saved = true;
        }
    }

    // Cursor's model per tier.
    if is_set(env, "RETASKABLE_CURSOR_MODELS") {
        term.said("Cursor's models", "RETASKABLE_CURSOR_MODELS")?;
    } else {
        let current = config::cursor_models(&cfg, env);
        let mut update = CursorModelsUpdate {
            top: current.top.clone(),
            implementer: current.implementer.clone(),
            scout: current.scout.clone(),
        };
        let mut changed = false;
        let tiers = [
            ("Cursor's model for planning and review (auto for auto)", 0),
            ("Cursor's model for building (auto for auto)", 1),
            ("Cursor's model for scouts and small jobs (auto for auto)", 2),
        ];
        for (label, tier) in tiers {
            let slot = match tier {
                0 => &mut update.top,
                1 => &mut update.implementer,
                _ => &mut update.scout,
            };
            let shown = slot.clone().unwrap_or_else(|| "auto".to_string());
            let mut typed = None;
            changed |= term.ask(label, &shown, true, |t| {
                let name = (!t.eq_ignore_ascii_case("auto")).then(|| t.to_string());
                saver.validate_cursor_models(&CursorModelsUpdate {
                    top: name.clone(),
                    ..Default::default()
                })?;
                typed = Some(name);
                Ok(())
            })?;
            if let Some(name) = typed {
                *slot = name;
            }
        }
        if changed {
            saver.save_cursor_models(&update)?;
            saved = true;
        }
    }

    // Network access and its password.
    if is_set(env, "RETASKABLE_NETWORK_PASSWORD") {
        term.said("Network access", "RETASKABLE_NETWORK_PASSWORD")?;
    } else {
        let stored = machine.network().unwrap_or_default();
        let has_hash = stored.password_hash.is_some();
        if let Some(on) = term.ask_on_off("Network access on or off", stored.enabled)? {
            let mut password = None;
            if on {
                password = ask_password(term, has_hash)?;
            }
            let enabled = on && (password.is_some() || has_hash);
            if on && !enabled {
                writeln!(term.out, "Network access needs a password, so it stays off.")?;
            }
            if enabled != stored.enabled || password.is_some() {
                saver.save_network(&NetworkUpdate { enabled, password })?;
                saved = true;
            }
        }
    }

    if saved {
        writeln!(term.out, "Saved. If Retaskable is running, restart it to use them.")?;
    }
    Ok(())
}

/// The password, asked again while it is too short; `None` when left empty.
fn ask_password(term: &mut Terminal, has_hash: bool) -> Result<Option<String>> {
    let hint = if has_hash {
        "Password, at least 8 characters (Enter keeps the current one)"
    } else {
        "Password, at least 8 characters (Enter for none)"
    };
    loop {
        write!(term.out, "{hint}: ")?;
        term.out.flush()?;
        let typed = (term.secret)(&mut *term.input)?;
        writeln!(term.out)?;
        let typed = typed.trim_end_matches(['\r', '\n']);
        if typed.is_empty() {
            return Ok(None);
        }
        if typed.encode_utf16().count() >= network::MIN_PASSWORD_LEN {
            return Ok(Some(typed.to_string()));
        }
        writeln!(
            term.out,
            "Password must be at least {} characters.",
            network::MIN_PASSWORD_LEN
        )?;
    }
}

/// Reads a line from the terminal with its echo turned off, the real `Terminal::secret`.
pub fn read_secret(input: &mut dyn BufRead) -> io::Result<String> {
    let _echo = EchoOff::new();
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line)
}

/// Turns the terminal's echo off until dropped.
struct EchoOff(#[cfg(windows)] Option<u32>);

#[cfg(not(windows))]
impl EchoOff {
    fn new() -> Self {
        let _ = std::process::Command::new("stty").arg("-echo").status();
        EchoOff()
    }
}

#[cfg(not(windows))]
impl Drop for EchoOff {
    fn drop(&mut self) {
        let _ = std::process::Command::new("stty").arg("echo").status();
    }
}

#[cfg(windows)]
impl EchoOff {
    fn new() -> Self {
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
        };
        // SAFETY: plain Win32 calls on the process's own stdin handle.
        unsafe {
            let handle = GetStdHandle(STD_INPUT_HANDLE);
            let mut mode = 0;
            if GetConsoleMode(handle, &mut mode) == 0 {
                return EchoOff(None);
            }
            SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT);
            EchoOff(Some(mode))
        }
    }
}

#[cfg(windows)]
impl Drop for EchoOff {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode};
        if let Some(mode) = self.0 {
            // SAFETY: restores the mode read in `new`.
            unsafe {
                SetConsoleMode(GetStdHandle(STD_INPUT_HANDLE), mode);
            }
        }
    }
}

#[cfg(test)]
mod tests;
