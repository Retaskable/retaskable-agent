//! `installerFor(build)` (installers/03-the-bundle-agent-bundle.md section 3.12), following
//! `agent/src/releaseInstall.ts`: the three concrete [`crate::release_update::Installer`]s that
//! let a release build replace itself once `release_update` has downloaded and checked a newer
//! one, one per OS the bundle ships for (section 3.12's table). [`relaunch_with_same_args`] and
//! [`relaunch_target`] are the plain relaunch section 3.8 describes, which `main.rs` falls back to
//! when nothing was ready to install or the install failed ("it relaunches itself as before").
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use crate::release_update::Installer;

/// One side of a signature comparison (`signatureRefusal`): whether it's validly signed and,
/// when it is, who signed it (an Authenticode certificate's subject, or a macOS Team ID).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    pub valid: bool,
    pub signer: Option<String>,
}

/// `signatureRefusal`: `None` when `downloaded` may be installed over `running`, else why not. A
/// `running` copy that isn't validly signed has nothing to compare (every build until
/// windows-release/mac-release sign them, section 3.12), so only the feed's SHA-256 — already
/// checked before this runs — guards it.
pub fn signature_refusal(running: &Signature, downloaded: &Signature) -> Option<String> {
    if !running.valid {
        return None;
    }
    if !downloaded.valid {
        return Some("the download was refused: it isn't signed, and this copy is".to_string());
    }
    if running.signer != downloaded.signer {
        return Some(format!(
            "the download was refused: it's signed by {}, not {} as this copy is",
            downloaded.signer.as_deref().unwrap_or("nobody"),
            running.signer.as_deref().unwrap_or("nobody"),
        ));
    }
    None
}

/// The one [`Installer`] for the OS this copy actually runs on. `exe_path` is the running
/// executable: the signature comparison's own side (Windows, macOS) and, on macOS, how its `.app`
/// is found.
pub fn installer_for(exe_path: &Path) -> Arc<dyn Installer> {
    #[cfg(target_os = "windows")]
    let installer: Arc<dyn Installer> = Arc::new(WindowsInstaller::new(exe_path.to_path_buf()));
    #[cfg(target_os = "macos")]
    let installer: Arc<dyn Installer> = Arc::new(MacInstaller::new(exe_path.to_path_buf()));
    #[cfg(all(unix, not(target_os = "macos")))]
    let installer: Arc<dyn Installer> = {
        let _ = exe_path;
        Arc::new(LinuxInstaller::new(std::env::var_os("APPIMAGE").map(PathBuf::from)))
    };
    installer
}

/// `exe_path` (the running copy), or, on Linux, `$APPIMAGE` when it's running as one: what
/// `main.rs` relaunches into when nothing was ready to install, or the install failed (section
/// 3.8's plain relaunch, the fallback section 3.12's "it relaunches itself as before" asks for).
pub fn relaunch_target(exe_path: &Path) -> PathBuf {
    #[cfg(all(unix, not(target_os = "macos")))]
    if let Some(appimage) = std::env::var_os("APPIMAGE") {
        return PathBuf::from(appimage);
    }
    exe_path.to_path_buf()
}

/// Starts `exe` with this process's own arguments (not waited on), detached with no window: the
/// way macOS and Linux's `install` restarts into what they just put in place, and `main.rs`'s
/// fallback when there's nothing to install.
pub fn relaunch_with_same_args(exe: &Path) -> Result<bool, String> {
    let mut cmd = Command::new(exe);
    cmd.args(std::env::args().skip(1));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        cmd.creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS);
    }
    cmd.spawn()
        .map(|_| true)
        .map_err(|e| format!("starting {}: {e}", exe.display()))
}

// ---------------------------------------------------------------------------------------------
// Windows: the feed's file is the next installer, run silently once its signature is checked.
// ---------------------------------------------------------------------------------------------

#[cfg(target_os = "windows")]
pub struct WindowsInstaller {
    exe_path: PathBuf,
}

#[cfg(target_os = "windows")]
impl WindowsInstaller {
    pub fn new(exe_path: PathBuf) -> Self {
        Self { exe_path }
    }
}

#[cfg(target_os = "windows")]
impl Installer for WindowsInstaller {
    fn prepare(&self, file: &Path) -> Result<PathBuf, String> {
        let running = authenticode_signature(&self.exe_path)?;
        let downloaded = authenticode_signature(file)?;
        if let Some(reason) = signature_refusal(&running, &downloaded) {
            return Err(reason);
        }
        Ok(file.to_path_buf())
    }

    fn install(&self, prepared: &Path) -> Result<bool, String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        Command::new(prepared)
            .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
            .creation_flags(CREATE_NO_WINDOW | DETACHED_PROCESS)
            .spawn()
            .map(|_| true)
            .map_err(|e| format!("running {}: {e}", prepared.display()))
    }
}

/// `Get-AuthenticodeSignature`'s status and signer certificate subject for `path` (section
/// 3.12's Windows row). `Err` only when PowerShell itself couldn't be run; a file that's missing
/// or isn't signed comes back as an invalid [`Signature`] instead, which `prepare` turns into the
/// right refusal (or none).
#[cfg(target_os = "windows")]
fn authenticode_signature(path: &Path) -> Result<Signature, String> {
    // The path travels in an environment variable, not spliced into the script: PowerShell also
    // treats the typographic quotes '\u{2018}', '\u{2019}', '\u{201a}' and '\u{201b}' as single-quote
    // delimiters, which a path containing one could use to break out of a quoted string literal.
    const SCRIPT: &str = "$s = Get-AuthenticodeSignature -LiteralPath $env:RETASKABLE_SIG_PATH; \
        Write-Output $s.Status; Write-Output $s.SignerCertificate.Subject";
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", SCRIPT])
        .env("RETASKABLE_SIG_PATH", path)
        .output()
        .map_err(|e| format!("running powershell: {e}"))?;
    Ok(parse_authenticode_signature(&String::from_utf8_lossy(&output.stdout)))
}

/// `authenticode_signature`'s own two lines (the status, then the signer certificate's subject),
/// pulled apart so the parsing is testable without PowerShell.
#[cfg(any(target_os = "windows", test))]
fn parse_authenticode_signature(stdout: &str) -> Signature {
    let mut lines = stdout.lines().map(str::trim);
    let valid = lines.next() == Some("Valid");
    let signer = lines.next().filter(|s| !s.is_empty()).map(str::to_string);
    Signature {
        valid,
        signer: if valid { signer } else { None },
    }
}

// ---------------------------------------------------------------------------------------------
// macOS: the feed's file is the `.app` zipped; unpacked, signature-checked, then swapped in.
// ---------------------------------------------------------------------------------------------

#[cfg(target_os = "macos")]
pub struct MacInstaller {
    exe_path: PathBuf,
    app_path: Option<PathBuf>,
}

#[cfg(target_os = "macos")]
impl MacInstaller {
    pub fn new(exe_path: PathBuf) -> Self {
        let app_path = exe_path
            .ancestors()
            .find(|p| p.extension().map(|e| e == "app").unwrap_or(false))
            .map(PathBuf::from);
        Self { exe_path, app_path }
    }
}

#[cfg(target_os = "macos")]
impl Installer for MacInstaller {
    fn cannot(&self) -> Option<String> {
        self.app_path
            .is_none()
            .then(|| "it isn't running from an app".to_string())
    }

    fn prepare(&self, file: &Path) -> Result<PathBuf, String> {
        let dir = file.parent().unwrap_or_else(|| Path::new(".")).join("app");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        let status = Command::new("ditto")
            .arg("-x")
            .arg("-k")
            .arg(file)
            .arg(&dir)
            .status()
            .map_err(|e| format!("running ditto: {e}"))?;
        if !status.success() {
            return Err(format!("unpacking {} with ditto failed", file.display()));
        }
        let apps: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| format!("reading {}: {e}", dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "app").unwrap_or(false))
            .collect();
        let [new_app] = apps.as_slice() else {
            return Err(format!("{} didn't unpack to one .app", file.display()));
        };
        if let Some(app_path) = &self.app_path {
            let running = codesign_signature(app_path);
            let downloaded = codesign_signature(new_app);
            if let Some(reason) = signature_refusal(&running, &downloaded) {
                return Err(reason);
            }
        }
        Ok(new_app.clone())
    }

    fn install(&self, prepared: &Path) -> Result<bool, String> {
        let app_path = self
            .app_path
            .as_ref()
            .ok_or_else(|| "it isn't running from an app".to_string())?;
        let old = app_path.with_extension("app.old");
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(app_path, &old).map_err(|e| format!("moving {} aside: {e}", app_path.display()))?;
        if std::fs::rename(prepared, app_path).is_err() {
            copy_dir_all(prepared, app_path).map_err(|e| format!("copying {} into place: {e}", prepared.display()))?;
            let _ = std::fs::remove_dir_all(prepared);
        }
        let _ = std::fs::remove_dir_all(&old);
        let exe_name = self
            .exe_path
            .file_name()
            .ok_or_else(|| "the running executable has no file name".to_string())?;
        relaunch_with_same_args(&app_path.join("Contents").join("MacOS").join(exe_name))
    }
}

/// `codesign`'s own validity (`--verify --deep --strict`) and, when valid, its Team ID
/// (`-dv`'s `TeamIdentifier=`), for the signature comparison `prepare` runs.
#[cfg(target_os = "macos")]
fn codesign_signature(path: &Path) -> Signature {
    let valid = Command::new("codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !valid {
        return Signature {
            valid: false,
            signer: None,
        };
    }
    let signer = Command::new("codesign")
        .arg("-dv")
        .arg(path)
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .find_map(|l| l.strip_prefix("TeamIdentifier=").map(str::to_string))
        });
    Signature {
        valid: true,
        signer: signer.filter(|t| t != "not set"),
    }
}

/// A plain recursive copy, for moving the new `.app` into place when it's on another volume from
/// the running one (`std::fs::rename` across volumes fails).
#[cfg(target_os = "macos")]
fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else {
            std::fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Linux: the feed's file is the AppImage itself; no signature, so the hash is its only check.
// ---------------------------------------------------------------------------------------------

#[cfg(all(unix, not(target_os = "macos")))]
pub struct LinuxInstaller {
    appimage_path: Option<PathBuf>,
}

#[cfg(all(unix, not(target_os = "macos")))]
impl LinuxInstaller {
    pub fn new(appimage_path: Option<PathBuf>) -> Self {
        Self { appimage_path }
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
impl Installer for LinuxInstaller {
    fn cannot(&self) -> Option<String> {
        self.appimage_path
            .is_none()
            .then(|| "it isn't running as an AppImage".to_string())
    }

    fn prepare(&self, file: &Path) -> Result<PathBuf, String> {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(file)
            .map_err(|e| format!("reading {}: {e}", file.display()))?
            .permissions();
        perms.set_mode(perms.mode() | 0o111);
        std::fs::set_permissions(file, perms).map_err(|e| format!("making {} executable: {e}", file.display()))?;
        Ok(file.to_path_buf())
    }

    fn install(&self, prepared: &Path) -> Result<bool, String> {
        let appimage = self
            .appimage_path
            .as_ref()
            .ok_or_else(|| "it isn't running as an AppImage".to_string())?;
        let mut new_path = appimage.as_os_str().to_os_string();
        new_path.push(".new");
        let new_path = PathBuf::from(new_path);
        std::fs::copy(prepared, &new_path)
            .map_err(|e| format!("copying {} to {}: {e}", prepared.display(), new_path.display()))?;
        std::fs::rename(&new_path, appimage)
            .map_err(|e| format!("renaming {} over {}: {e}", new_path.display(), appimage.display()))?;
        relaunch_with_same_args(appimage)
    }
}
