//! Whether the agent draws a tray (tray/04-the-tray-agent.md sections 4.4 and 4.6 "Without a tray"): the pure
//! decision, made before anything is started. A helper that doesn't start, isn't ready in 10 seconds or stops
//! later is handled where it runs (`run`).
use std::path::PathBuf;

use crate::env::Env;
use crate::tags::OsKind;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrayMode {
    /// Spawn this helper and run the tray.
    Tray(PathBuf),
    /// Run with the plain console; the reason is said in it.
    Headless(String),
}

/// Why the agent runs without a tray from the start, when it does (the first three reasons; a missing helper is not one).
pub(crate) fn no_tray_from_start(args: &[String], env: &Env, os: OsKind) -> Option<String> {
    if args.iter().any(|a| a == "--no-tray") {
        return Some("--no-tray".into());
    }
    if env
        .get("RETASKABLE_NO_TRAY")
        .is_some_and(|v| !matches!(v.trim(), "" | "0"))
    {
        return Some("RETASKABLE_NO_TRAY is set".into());
    }
    if os == OsKind::Linux && env.get("DISPLAY").is_none() && env.get("WAYLAND_DISPLAY").is_none() {
        return Some("no DISPLAY or WAYLAND_DISPLAY".into());
    }
    None
}

/// `args` are the command line after the program name; `helper` is what `tray::helper_path` found.
pub fn tray_mode(args: &[String], env: &Env, os: OsKind, helper: Option<PathBuf>) -> TrayMode {
    if let Some(why) = no_tray_from_start(args, env, os) {
        return TrayMode::Headless(why);
    }
    match helper {
        Some(path) => TrayMode::Tray(path),
        None => TrayMode::Headless("no tray helper for this computer".into()),
    }
}

/// Whether the terminal menu is drawn (section 4.7): never without a terminal on both stdin and stdout, never with
/// `--no-tui` or `RETASKABLE_TUI=0`; otherwise with `--tui` or `RETASKABLE_TUI=1`, or whenever there is no tray
/// from the start.
pub fn use_tui(args: &[String], env: &Env, os: OsKind, terminals: bool) -> bool {
    let has = |f: &str| args.iter().any(|a| a == f);
    let var = env.get("RETASKABLE_TUI").map(|v| v.trim().to_string());
    if !terminals || has("--no-tui") {
        return false;
    }
    if has("--tui") {
        return true;
    }
    match var.as_deref() {
        Some("0") => false,
        Some("") | None => no_tray_from_start(args, env, os).is_some(),
        Some(_) => true,
    }
}
