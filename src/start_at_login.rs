//! Start at login, Linux only (installers/03-the-bundle-agent-bundle.md section 3.14, following
//! `agent/src/startAtLogin.ts`): a release build adds itself to XDG autostart on its first run,
//! so it starts when the user signs in. Production and Staging each have their own entry, so
//! either can be on without the other. macOS and Windows's own start-at-login are the desktop
//! app's (desktop-app.md section 3) and the installer's, not this agent's (06-macos-and-linux.md
//! section 6.1). A checkout has none of this.
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::release::{Build, Environment, release_name};

/// A desktop entry's `Exec=`, quoted as the Desktop Entry Specification asks: `\`, `"`, `` ` ``
/// and `$` backslash-escaped, `%` doubled, the whole value quoted.
pub fn desktop_exec(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' | '"' | '`' | '$' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// What starts this copy: on Linux the AppImage itself (`$APPIMAGE`), not the executable inside
/// its mount.
fn start_path<'a>(appimage: Option<&'a str>, exec_path: &'a str) -> &'a str {
    appimage.unwrap_or(exec_path)
}

/// This build's autostart entry: `retaskable(-staging).desktop` in `$XDG_CONFIG_HOME/autostart`
/// (or `~/.config/autostart`), naming what starts this copy.
pub fn login_entry(
    build: &Build,
    config_home: Option<&str>,
    appimage: Option<&str>,
    home: &str,
    exec_path: &str,
) -> (PathBuf, String) {
    let staging = build.environment == Environment::Staging;
    let exe = start_path(appimage, exec_path);
    let config = config_home
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(home).join(".config"));
    let text = format!(
        "[Desktop Entry]\nType=Application\nName={}\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        release_name(build.environment),
        desktop_exec(exe)
    );
    let file_name = if staging {
        "retaskable-staging.desktop"
    } else {
        "retaskable.desktop"
    };
    (config.join("autostart").join(file_name), text)
}

/// Adds or removes this build's autostart entry, and the "at start" logic that turns it on by
/// default on the first run only (installers/03-the-bundle-agent-bundle.md section 3.14).
pub struct StartAtLogin {
    file: PathBuf,
    text: String,
}

impl StartAtLogin {
    pub fn new(file: PathBuf, text: String) -> Self {
        Self { file, text }
    }

    pub fn is_on(&self) -> bool {
        self.file.exists()
    }

    pub fn set(&self, on: bool) -> Result<()> {
        if on {
            if let Some(parent) = self.file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&self.file, &self.text)?;
        } else {
            match std::fs::remove_file(&self.file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// At start: the first run turns it on, remembered by `start-at-login` in the data folder so
    /// a later untick stays off after a restart; an entry that's already on is written again, so
    /// it names where this copy is now.
    pub fn at_start(&self, data_dir: &Path) -> Result<()> {
        let marker = data_dir.join("start-at-login");
        let first = !marker.exists();
        if first {
            std::fs::create_dir_all(data_dir)?;
            std::fs::write(&marker, "Start at login was set up on the first run.\n")?;
        }
        if first || self.is_on() {
            self.set(true)?;
        }
        Ok(())
    }
}
