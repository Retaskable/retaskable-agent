//! The tray icon in each state (tray/04-the-tray-agent.md section 4.6, "The icon"): the tanooki at
//! 32 px, on grey while not linked, else on its Sun tile with an amber, red or green dot; Staging's
//! carry the white "S" tile. Prebuilt PNGs under `assets/tray/`, drawn by the TypeScript agent's
//! `trayIcons.ts`. A PNG on macOS and Linux, an ICO holding that PNG on Windows. A release build
//! reads `icons/<state>.ico` or `.png` from its `bundle_dir`; a checkout uses the embedded ones.
use std::path::Path;

use crate::release::{Build, Environment, bundle_dir};
use crate::tags::OsKind;

#[cfg(test)]
mod tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    NotLinked,
    Waiting,
    Offline,
    Connected,
}

pub const ICON_STATES: [TrayState; 4] = [
    TrayState::NotLinked,
    TrayState::Connected,
    TrayState::Waiting,
    TrayState::Offline,
];

impl TrayState {
    /// The icon file's stem, as `icons/` names it.
    pub fn name(self) -> &'static str {
        match self {
            TrayState::NotLinked => "not_linked",
            TrayState::Waiting => "waiting",
            TrayState::Offline => "offline",
            TrayState::Connected => "connected",
        }
    }
}

/// A state's icon file in `icons/`: `connected.ico` on Windows, `connected.png` elsewhere.
pub fn icon_file_name(state: TrayState, os: OsKind) -> String {
    format!("{}.{}", state.name(), if os == OsKind::Windows { "ico" } else { "png" })
}

fn png(state: TrayState, environment: Environment) -> &'static [u8] {
    macro_rules! icon {
        ($name:literal) => {
            include_bytes!(concat!("../../assets/tray/", $name, ".png")) as &[u8]
        };
    }
    match (environment, state) {
        (Environment::Production, TrayState::NotLinked) => icon!("not_linked"),
        (Environment::Production, TrayState::Waiting) => icon!("waiting"),
        (Environment::Production, TrayState::Offline) => icon!("offline"),
        (Environment::Production, TrayState::Connected) => icon!("connected"),
        (Environment::Staging, TrayState::NotLinked) => icon!("not_linked-staging"),
        (Environment::Staging, TrayState::Waiting) => icon!("waiting-staging"),
        (Environment::Staging, TrayState::Offline) => icon!("offline-staging"),
        (Environment::Staging, TrayState::Connected) => icon!("connected-staging"),
    }
}

/// A one-image ICO whose image is a 32-pixel PNG (Windows Vista and later read these).
fn ico(png: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(22 + png.len());
    out.extend_from_slice(&[0, 0, 1, 0, 1, 0]); // reserved, icon, one image
    out.extend_from_slice(&[32, 32, 0, 0]); // size, no palette, reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
    out.extend_from_slice(&(png.len() as u32).to_le_bytes());
    out.extend_from_slice(&22u32.to_le_bytes()); // where the image starts
    out.extend_from_slice(png);
    out
}

/// A state's icon for an OS: the PNG, or on Windows an ICO wrapping it.
pub fn tray_icon(state: TrayState, environment: Environment, os: OsKind) -> Vec<u8> {
    let png = png(state, environment);
    if os == OsKind::Windows { ico(png) } else { png.to_vec() }
}

/// The icon to show: a release build reads `icons/<state>` from its `bundle_dir`, a checkout (no
/// `build`) or a missing file uses the embedded one.
pub fn load_tray_icon(state: TrayState, build: Option<&Build>, exe: &Path, os: OsKind) -> Vec<u8> {
    match build {
        Some(build) => std::fs::read(bundle_dir(exe).join("icons").join(icon_file_name(state, os)))
            .unwrap_or_else(|_| tray_icon(state, build.environment, os)),
        None => tray_icon(state, Environment::Production, os),
    }
}
