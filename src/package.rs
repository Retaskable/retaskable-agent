//! Packaging a built bundle for distribution, Linux only (installers/06-macos-and-linux.md
//! section 6.2, following `scripts/package.ts` in Retaskable.com): an AppImage — the bundle's
//! files wrapped in an AppDir (an `AppRun` symlink to the executable and a desktop entry,
//! installers/03-the-bundle-agent-bundle.md section 3.15), squashed with `mksquashfs`
//! (squashfs-tools) and prefixed with the pinned AppImage runtime. macOS's package is the
//! desktop app's now (section 6.1), and Windows's installer is a step of its own, so this builds
//! only `linux-x64`.
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::app_menu::{app_id, desktop_entry};
use crate::release::Build;
use crate::release_update::sha256_file;

/// The pinned AppImage runtime (type 2, x86_64): `scripts/package.ts`'s `APPIMAGE_RUNTIME`. A
/// mismatched download is refused rather than trusted (same reasoning as `release_update`'s own
/// pinned-hash check).
const APPIMAGE_RUNTIME_URL: &str =
    "https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64";
const APPIMAGE_RUNTIME_SHA256: &str = "2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d";

/// `Retaskable-x86_64.AppImage` or `Retaskable-Staging-x86_64.AppImage`, matching
/// `packageName()` in `scripts/package.ts`. `None` for any target but `linux-x64`: that's all
/// this builds (section 6.1/6.2).
pub fn package_name(build: &Build) -> Result<String> {
    if build.os != "linux-x64" {
        bail!(
            "package only builds the Linux AppImage (build.os is {:?}): macOS's package is the desktop app's \
             now (installers/06-macos-and-linux.md section 6.1) and Windows's installer is a step of its own",
            build.os
        );
    }
    Ok(format!(
        "{}-x86_64.AppImage",
        crate::release::release_name(build.environment).replace(' ', "-")
    ))
}

/// Copies `from` onto `to`, creating directories as it goes. Plain files only: `bundle`'s output
/// has no symlinks of its own to preserve.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to).with_context(|| format!("creating {}", to.display()))?;
    for entry in std::fs::read_dir(from).with_context(|| format!("reading {}", from.display()))? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)
                .with_context(|| format!("copying {} to {}", entry.path().display(), dest.display()))?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn make_symlink(original: &str, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(original, link).with_context(|| format!("linking {} to {original}", link.display()))
}

#[cfg(not(unix))]
fn make_symlink(_original: &str, link: &Path) -> Result<()> {
    bail!(
        "an AppImage's AppDir needs a symlink for {} (AppRun): built on Linux or macOS, not Windows",
        link.display()
    )
}

#[cfg(unix)]
fn make_executable(file: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("making {} executable", file.display()))
}

#[cfg(not(unix))]
fn make_executable(_file: &Path) -> Result<()> {
    Ok(())
}

/// Section 3.15's AppDir: the bundle's files, an `AppRun` symlink to `retaskable` and a desktop
/// entry naming it — no icon yet, since no copy of this agent has one to embed (the tray that
/// would supply one, 06-macos-and-linux.md section 6.2, isn't ported).
fn build_app_dir(bundle: &Path, build: &Build, dir: &Path) -> Result<()> {
    copy_dir(bundle, dir)?;
    make_symlink("retaskable", &dir.join("AppRun"))?;
    let id = app_id(build);
    std::fs::write(
        dir.join(format!("{id}.desktop")),
        desktop_entry(build, &id, "retaskable"),
    )
    .with_context(|| format!("writing {id}.desktop"))?;
    Ok(())
}

/// `mksquashfs app_dir -root-owned -noappend -comp zstd -quiet`, matching `scripts/package.ts`'s
/// `appImage()`.
fn make_squashfs(app_dir: &Path, squashfs: &Path) -> Result<()> {
    let status = Command::new("mksquashfs")
        .arg(app_dir)
        .arg(squashfs)
        .args(["-root-owned", "-noappend", "-comp", "zstd", "-quiet"])
        .status()
        .with_context(|| "running mksquashfs (needs squashfs-tools installed)".to_string())?;
    if !status.success() {
        bail!("mksquashfs failed ({status})");
    }
    Ok(())
}

fn download_runtime(url: &str) -> Result<Vec<u8>> {
    let response = reqwest::blocking::get(url).with_context(|| format!("downloading {url}"))?;
    if !response.status().is_success() {
        bail!("{url} answered {}", response.status());
    }
    Ok(response.bytes().with_context(|| format!("reading {url}"))?.to_vec())
}

/// The AppImage runtime, cached under `cache_dir` by its pinned hash's first 12 characters
/// (`scripts/package.ts`'s `appImageRuntime()`), downloaded once and verified against it every
/// time after — a cached file with the wrong bytes is refused, not silently re-downloaded.
fn cached_runtime_for(cache_dir: &Path, url: &str, sha256: &str) -> Result<Vec<u8>> {
    let file = cache_dir.join(format!("appimage-runtime-{}", &sha256[..12]));
    let data = match std::fs::read(&file) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let data = download_runtime(url)?;
            std::fs::create_dir_all(cache_dir).with_context(|| format!("creating {}", cache_dir.display()))?;
            std::fs::write(&file, &data).with_context(|| format!("writing {}", file.display()))?;
            data
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let got = crate::release_update::sha256_hex(&data);
    if got != sha256 {
        bail!("{} is {got}, not the pinned {sha256} ({url})", file.display());
    }
    Ok(data)
}

/// The pinned AppImage runtime, cached under `cache_dir`.
pub fn cached_runtime(cache_dir: &Path) -> Result<Vec<u8>> {
    cached_runtime_for(cache_dir, APPIMAGE_RUNTIME_URL, APPIMAGE_RUNTIME_SHA256)
}

fn unique_scratch_name(prefix: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

/// Builds `bundle` into the AppImage at `out_file`: an AppDir, squashed, prefixed with the
/// cached runtime, made executable.
pub fn make_appimage(bundle: &Path, build: &Build, out_file: &Path, cache_dir: &Path) -> Result<()> {
    let work = std::env::temp_dir().join(unique_scratch_name("retaskable-appdir"));
    let result = (|| -> Result<()> {
        let app_dir = work.join("AppDir");
        build_app_dir(bundle, build, &app_dir)?;
        let squashfs = work.join("app.squashfs");
        make_squashfs(&app_dir, &squashfs)?;
        let runtime = cached_runtime(cache_dir)?;
        let squashfs_bytes = std::fs::read(&squashfs).with_context(|| format!("reading {}", squashfs.display()))?;
        if let Some(parent) = out_file.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(out_file, [runtime, squashfs_bytes].concat())
            .with_context(|| format!("writing {}", out_file.display()))?;
        make_executable(out_file)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

/// Packages `bundle` into `out_dir`, naming the file `package_name` picks. Answers the file's
/// path and its SHA-256, for whoever publishes it next.
pub fn make_package(bundle: &Path, build: &Build, out_dir: &Path, cache_dir: &Path) -> Result<(PathBuf, String)> {
    let file = out_dir.join(package_name(build)?);
    make_appimage(bundle, build, &file, cache_dir)?;
    let sha256 = sha256_file(&file)?;
    Ok((file, sha256))
}
