//! Which UI the agent starts (tray/04-the-tray-agent.md sections 4.4, 4.6 "Without a tray" and 4.7 "When"): the
//! pure decision, made before anything is started. A helper that fails to start or stops later is not decided
//! here: it falls back to the plain console, never to the menu.
use crate::env::Env;
use crate::tags::OsKind;
use crate::tray::mode::no_tray_from_start;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiMode {
    Tray,
    TerminalMenu,
    Console,
}

fn os_kind(os: &str) -> OsKind {
    match os {
        "linux" => OsKind::Linux,
        "windows" => OsKind::Windows,
        "macos" => OsKind::Mac,
        _ => OsKind::Other,
    }
}

/// Why there is no tray from the start, if there isn't one: `--no-tray`, `RETASKABLE_NO_TRAY`, a Linux with no
/// `DISPLAY` or `WAYLAND_DISPLAY`, or no helper for this OS.
fn no_tray_cause(args: &[String], env: &Env, os: &str, has_helper: bool) -> Option<String> {
    no_tray_from_start(args, env, os_kind(os)).or_else(|| (!has_helper).then(|| "no tray helper for this OS".into()))
}

/// `os` is `std::env::consts::OS`; `interactive` is stdin and stdout both being terminals.
pub fn ui_mode(args: &[String], env: &Env, os: &str, has_helper: bool, interactive: bool) -> UiMode {
    let has = |f: &str| args.iter().any(|a| a == f);
    let var = env.get("RETASKABLE_TUI").map(|v| v.trim().to_string());
    if has("--no-tui") || var.as_deref() == Some("0") {
        return UiMode::Console;
    }
    let menu = if interactive {
        UiMode::TerminalMenu
    } else {
        UiMode::Console
    };
    if has("--tui") || var.as_deref() == Some("1") {
        return menu;
    }
    if no_tray_cause(args, env, os, has_helper).is_some() {
        return menu;
    }
    UiMode::Tray
}

/// The console line saying why the agent runs without a tray, or `None` when it has one.
pub fn no_tray_reason(args: &[String], env: &Env, os: &str, has_helper: bool) -> Option<String> {
    no_tray_cause(args, env, os, has_helper).map(|why| format!("No tray: {why}"))
}
