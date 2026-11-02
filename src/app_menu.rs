//! The app menu, Linux only (installers/03-the-bundle-agent-bundle.md section 3.17, following
//! `agent/src/appMenu.ts`): an AppImage adds itself to the desktop's app menu on its first run, a
//! desktop entry in `$XDG_DATA_HOME/applications` naming the AppImage, with its 256-pixel icon
//! copied out of its mount into the icon theme's folder. No root. Production and Staging each
//! have their own. A copy that isn't an AppImage (a checkout, a loose folder, Windows, macOS) has
//! none of this.
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::release::{Build, release_name};
use crate::start_at_login::desktop_exec;

/// The name its desktop entry and icon go by: `retaskable` or `retaskable-staging`.
pub fn app_id(build: &Build) -> String {
    release_name(build.environment).to_lowercase().replace(' ', "-")
}

/// A desktop entry for this build, `exec` quoted the same way `start_at_login::login_entry`'s is.
pub(crate) fn desktop_entry(build: &Build, icon: &str, exec: &str) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={}\nComment=Runs your Retaskable backlog on this computer\nExec={exec}\nIcon={icon}\nTerminal=false\nCategories=Development;\n",
        release_name(build.environment)
    )
}

/// The app menu's entry for this build: where it goes, its text, and its icon's source (the
/// AppImage's own mount) and destination (the icon theme's folder).
pub struct MenuEntry {
    pub file: PathBuf,
    pub text: String,
    pub icon_from: PathBuf,
    pub icon_to: PathBuf,
}

/// The app menu's entry for this copy, or `None` when it isn't running as an AppImage (no
/// `$APPIMAGE`). The icon is the one at the AppImage's own root (`$APPDIR`, its mount), or beside
/// the executable when that's missing.
pub fn menu_entry(
    build: &Build,
    appimage: Option<&str>,
    appdir: Option<&str>,
    data_home: Option<&str>,
    home: &str,
    exec_path: &Path,
) -> Option<MenuEntry> {
    let appimage = appimage?;
    let id = app_id(build);
    let data = data_home
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(home).join(".local").join("share"));
    let icon_dir = appdir
        .map(PathBuf::from)
        .unwrap_or_else(|| exec_path.parent().unwrap_or(Path::new(".")).to_path_buf());
    Some(MenuEntry {
        file: data.join("applications").join(format!("{id}.desktop")),
        text: desktop_entry(build, &id, &desktop_exec(appimage)),
        icon_from: icon_dir.join(format!("{id}.png")),
        icon_to: data
            .join("icons")
            .join("hicolor")
            .join("256x256")
            .join("apps")
            .join(format!("{id}.png")),
    })
}

/// At start: the first run (no `app-menu` marker in the data folder, which it then writes) adds
/// the entry; after that, an entry that's there is written again, so it names where the AppImage
/// is now, and one the user removed stays removed. Answers whether it wrote it.
pub fn add_to_app_menu(entry: &MenuEntry, data_dir: &Path) -> Result<bool> {
    let marker = data_dir.join("app-menu");
    let first = !marker.exists();
    if !first && !entry.file.exists() {
        return Ok(false);
    }
    if let Some(parent) = entry.file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&entry.file, &entry.text)?;
    if entry.icon_from.exists() {
        if let Some(parent) = entry.icon_to.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&entry.icon_from, &entry.icon_to)?;
    }
    if first {
        std::fs::create_dir_all(data_dir)?;
        std::fs::write(&marker, "The app menu's entry was added on the first run.\n")?;
    }
    Ok(true)
}
