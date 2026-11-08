//! The tray helper binary (tray/04-the-tray-agent.md sections 4.5 and 4.6): `systray2`'s prebuilt
//! systray-portable helper for each OS, found where a release build ships it (beside the
//! executable, or a `.app`'s `Contents/Resources/`) and placed there by `bin/bundle.rs` from the
//! package's npm tarball, at a pinned version and checked against a pinned sha256.
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config;
use crate::env::Env;
use crate::release::{Build, bundle_dir};
use crate::tags::OsKind;

#[cfg(test)]
mod tests;

/// The `systray2` version the helper is taken from, and the sha256 of its npm tarball.
pub const SYSTRAY2_VERSION: &str = "2.1.4";
pub const SYSTRAY2_SHA256: &str = "24a176933952c4db79026dcec25c7acf48e5c1cee5d94bc6b87740060bc8ed00";

/// The helper's file name for an OS, as `systray2`'s `traybin/` names it; `None` where there's none.
pub fn helper_file_name(os: OsKind) -> Option<&'static str> {
    match os {
        OsKind::Windows => Some("tray_windows_release.exe"),
        OsKind::Mac => Some("tray_darwin_release"),
        OsKind::Linux => Some("tray_linux_release"),
        OsKind::Other => None,
    }
}

/// Where the helper is: a release build's copy beside the executable (or in a `.app`'s
/// `Contents/Resources/`); in a checkout `RETASKABLE_TRAY_HELPER`, else a fetched copy in
/// `<data dir>/tray/`. `None` when this OS has no helper or the file isn't there.
pub fn helper_path(build: Option<&Build>, exe: &Path, env: &Env, os: OsKind) -> Option<PathBuf> {
    let name = helper_file_name(os)?;
    let path = match build {
        Some(_) => bundle_dir(exe).join(name),
        None => match env.get("RETASKABLE_TRAY_HELPER") {
            Some(p) => PathBuf::from(p),
            None => config::data_dir(env).join("tray").join(name),
        },
    };
    path.is_file().then_some(path)
}

pub fn tarball_url() -> String {
    format!("https://registry.npmjs.org/systray2/-/systray2-{SYSTRAY2_VERSION}.tgz")
}

/// The OS's helper out of `systray2`'s npm tarball, refused unless the tarball's sha256 is `sha256`.
pub fn unpack_helper(tgz: &[u8], sha256: &str, os: OsKind) -> Result<Vec<u8>, String> {
    let name = helper_file_name(os).ok_or("there's no tray helper for this OS")?;
    let got: String = Sha256::digest(tgz).iter().map(|b| format!("{b:02x}")).collect();
    if !got.eq_ignore_ascii_case(sha256) {
        return Err(format!(
            "the systray2 tarball's sha256 is {got}, not the pinned {sha256}"
        ));
    }
    let wanted = Path::new("package/traybin").join(name);
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tgz));
    for entry in archive
        .entries()
        .map_err(|e| format!("reading the systray2 tarball: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("reading the systray2 tarball: {e}"))?;
        if entry.path().map_err(|e| e.to_string())? == wanted {
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| format!("reading {name}: {e}"))?;
            return Ok(bytes);
        }
    }
    Err(format!("the systray2 tarball has no {}", wanted.display()))
}

/// Writes the helper into `dir` under its own name, executable on Unix; returns its path.
pub fn place_helper(dir: &Path, bytes: &[u8], os: OsKind) -> Result<PathBuf, String> {
    let name = helper_file_name(os).ok_or("there's no tray helper for this OS")?;
    let path = dir.join(name);
    std::fs::write(&path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("making {} executable: {e}", path.display()))?;
    }
    Ok(path)
}
