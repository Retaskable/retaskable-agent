//! Retaskable's own copies of the GitHub CLI and, on Windows, git (`agent/src/ownGh.ts`, `ownGit.ts`): the pinned
//! release fetched into the data folder, checked against its SHA-256, unpacked with the system's own `tar`, and put
//! first on the agent's own PATH only, so its `gh` and `git` calls and its sessions find it and nothing else on the
//! computer changes. No admin password: nothing is written outside the data folder.
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::run::Ran;

/// The GitHub CLI release fetched, each build with its SHA-256 (from `gh_<version>_checksums.txt`). A newer one
/// comes with an agent update.
const GH_VERSION: &str = "2.103.0";
const GH_BUILDS: &[(&str, &str, &str)] = &[
    (
        "macos-arm64",
        "gh_2.103.0_macOS_arm64.zip",
        "54be566e9f614bea5de8021e95b9ad33dfee3b9d2ea120c9c294c00defaa5014",
    ),
    (
        "macos-x64",
        "gh_2.103.0_macOS_amd64.zip",
        "64d1734941f104f996271c820daa50e96902c2618f9b4c1e63c41519517a9c6b",
    ),
    (
        "windows-arm64",
        "gh_2.103.0_windows_arm64.zip",
        "bc8c27c396c683a7a823446fd0c16b923e60ec3fac5d26a875c646865d513a48",
    ),
    (
        "windows-x64",
        "gh_2.103.0_windows_amd64.zip",
        "80c846a5b8432672ca6373b34ce9de998c9686aaa1885c845f2e1ee81f713726",
    ),
    (
        "linux-arm64",
        "gh_2.103.0_linux_arm64.tar.gz",
        "7f322df34e624d14f5ec0c4acb121fd6dfa14392fdbbd1066aa8e0a05ac3721e",
    ),
    (
        "linux-x64",
        "gh_2.103.0_linux_amd64.tar.gz",
        "fdd768eb70f6409f80fcfa9f3a1c0adf0fc98d827ab3ce0bed2ac22762c5d786",
    ),
];

/// The MinGit release fetched (Git for Windows' portable build).
const MINGIT_TAG: &str = "v2.56.0.windows.2";
const MINGIT_BUILDS: &[(&str, &str, &str)] = &[
    (
        "x64",
        "MinGit-2.56.0.2-64-bit.zip",
        "da35e72aa21c005a5a0d298cfbae110bc1609a815730ea0dde84b01a1b3cd3be",
    ),
    (
        "arm64",
        "MinGit-2.56.0.2-arm64.zip",
        "38b33dc6024026e3315cf88ab2cfea65205bbd7bb3a8e824bd21c8ad4fe609a7",
    ),
];

/// A pinned download: its file name, SHA-256 and address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Build {
    pub name: String,
    pub sha256: String,
    pub url: String,
}

/// `os`: `linux`, `macos` or `windows`; `arch`: `std::env::consts::ARCH`. arm64 has its own build, anything else takes
/// the 64-bit one.
pub fn gh_build(os: &str, arch: &str) -> Option<Build> {
    let key = format!("{os}-{}", if arch == "aarch64" { "arm64" } else { "x64" });
    let (_, name, sha) = GH_BUILDS.iter().find(|(k, ..)| *k == key)?;
    Some(Build {
        name: name.to_string(),
        sha256: sha.to_string(),
        url: format!("https://github.com/cli/cli/releases/download/v{GH_VERSION}/{name}"),
    })
}

/// MinGit's build for this processor.
pub fn mingit_build(arch: &str) -> Build {
    let key = if arch == "aarch64" { "arm64" } else { "x64" };
    let (_, name, sha) = MINGIT_BUILDS
        .iter()
        .find(|(k, ..)| *k == key)
        .expect("both builds are listed");
    Build {
        name: name.to_string(),
        sha256: sha.to_string(),
        url: format!("https://github.com/git-for-windows/git/releases/download/{MINGIT_TAG}/{name}"),
    }
}

/// `<data>/gh` and `<data>/git`.
pub fn own_gh_dir(data: &Path) -> PathBuf {
    data.join("gh")
}
pub fn own_git_dir(data: &Path) -> PathBuf {
    data.join("git")
}

fn gh_file(windows: bool) -> &'static str {
    if windows { "gh.exe" } else { "gh" }
}

/// How the bytes of a download arrive, and how an archive is unpacked. Replaced in tests.
pub type DownloadFn = dyn Fn(&str) -> Result<Vec<u8>, String> + Send + Sync;
pub type ExtractFn = dyn Fn(&Path, &Path) -> Result<(), String> + Send + Sync;

/// A download's bytes, given up on after ten minutes.
pub fn download_bytes(url: &str) -> Result<Vec<u8>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let res = client.get(url).send().map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("the download answered {}", res.status().as_u16()));
    }
    res.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
}

/// Unpacks a zip or tarball with the system's own `tar`: Windows' (bsdtar, in System32 since Windows 10), macOS's
/// (bsdtar, which reads zips too) or Linux's.
pub fn extract_archive(
    run: &dyn Fn(&str, &[String], Duration) -> Ran,
    archive: &Path,
    to: &Path,
) -> Result<(), String> {
    let tar = if cfg!(windows) {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        Path::new(&root)
            .join("System32")
            .join("tar.exe")
            .to_string_lossy()
            .into_owned()
    } else {
        "tar".to_string()
    };
    let args = vec![
        "-xf".into(),
        archive.to_string_lossy().into_owned(),
        "-C".into(),
        to.to_string_lossy().into_owned(),
    ];
    let Ran {
        code, stdout, stderr, ..
    } = run(&tar, &args, Duration::from_secs(300));
    if code == Some(0) {
        return Ok(());
    }
    let said = if stderr.trim().is_empty() { stdout } else { stderr };
    Err(format!("couldn't unpack it: {}", said.trim()))
}

/// Fetches a pinned archive into `dir`: downloaded, checked against its SHA-256, unpacked beside it and moved into
/// place, so a copy that's there is whole. `keep` picks the folder inside the unpacked archive that becomes `dir`, or
/// says what's missing. Anything left half-done goes.
pub fn fetch_pinned(
    dir: &Path,
    build: &Build,
    publisher: &str,
    download: &dyn Fn(&str) -> Result<Vec<u8>, String>,
    extract: &dyn Fn(&Path, &Path) -> Result<(), String>,
    keep: impl Fn(&Path) -> Result<PathBuf, String>,
) -> Result<(), String> {
    let bytes = download(&build.url)?;
    let sha: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if sha != build.sha256 {
        return Err(format!(
            "the download of {} isn't the one {publisher} published, so it wasn't used.",
            build.name
        ));
    }
    let parent = dir.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let archive = parent.join(&build.name);
    let mut tmp = dir.as_os_str().to_owned();
    tmp.push(".part");
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        std::fs::write(&archive, &bytes).map_err(|e| e.to_string())?;
        extract(&archive, &tmp)?;
        let kept = keep(&tmp)?;
        let _ = std::fs::remove_dir_all(dir);
        std::fs::rename(kept, dir).map_err(|e| e.to_string())
    })();
    let _ = std::fs::remove_file(&archive);
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// The unpacked folder that holds `bin/gh`: the archive's top folder (macOS's and Linux's have one, Windows' none).
fn gh_root(unpacked: &Path, windows: bool) -> Result<PathBuf, String> {
    let has = |d: &Path| d.join("bin").join(gh_file(windows)).is_file();
    if has(unpacked) {
        return Ok(unpacked.to_path_buf());
    }
    let inner = std::fs::read_dir(unpacked)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_dir() && has(p));
    inner.ok_or_else(|| format!("gh_{GH_VERSION} has no bin/{}.", gh_file(windows)))
}

/// Fetches the pinned GitHub CLI into `<data>/gh` and puts it in use.
#[allow(clippy::too_many_arguments)]
pub fn fetch_own_gh(
    data: &Path,
    os: &str,
    arch: &str,
    build: Option<Build>,
    download: &dyn Fn(&str) -> Result<Vec<u8>, String>,
    extract: &dyn Fn(&Path, &Path) -> Result<(), String>,
    mut use_it: impl FnMut(&Path),
) -> Result<(), String> {
    let windows = os == "windows";
    let build = build
        .or_else(|| gh_build(os, arch))
        .ok_or_else(|| format!("The GitHub CLI has no build for {os}."))?;
    let dir = own_gh_dir(data);
    fetch_pinned(&dir, &build, "GitHub", download, extract, |tmp| gh_root(tmp, windows))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir.join("bin").join("gh"), std::fs::Permissions::from_mode(0o755));
    }
    use_it(&dir);
    Ok(())
}

/// Fetches MinGit into `<data>/git` (Windows).
pub fn fetch_own_git(
    data: &Path,
    arch: &str,
    build: Option<Build>,
    download: &dyn Fn(&str) -> Result<Vec<u8>, String>,
    extract: &dyn Fn(&Path, &Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let build = build.unwrap_or_else(|| mingit_build(arch));
    let dir = own_git_dir(data);
    fetch_pinned(&dir, &build, "Git for Windows", download, extract, |tmp| {
        if tmp.join("cmd").join("git.exe").is_file() {
            Ok(tmp.to_path_buf())
        } else {
            Err(format!("{} has no cmd\\git.exe.", build.name))
        }
    })?;
    Ok(dir)
}

/// Puts `dir` first on this process's PATH (and so its children's): the computer's PATH is untouched.
pub fn prepend_path(dir: &Path) {
    let old = std::env::var_os("PATH").unwrap_or_default();
    if std::env::split_paths(&old).any(|p| p == dir) {
        return;
    }
    let new = std::env::join_paths(std::iter::once(dir.to_path_buf()).chain(std::env::split_paths(&old)));
    if let Ok(new) = new {
        {
            use crate::secrets::Host;
            crate::secrets::ProcessHost.set("PATH", &new.to_string_lossy());
        }
    }
}

/// Puts the copy's `bin` (gh) first on PATH.
pub fn use_own_gh(dir: &Path) {
    prepend_path(&dir.join("bin"));
}

/// Puts the copy's `cmd` (git) first on PATH.
pub fn use_own_git(dir: &Path) {
    prepend_path(&dir.join("cmd"));
}

/// At start: a GitHub CLI fetched before is put in use, and MinGit too on Windows. True when there was a gh.
pub fn use_own_if_there(data: &Path) -> bool {
    let gh = own_gh_dir(data);
    let mut found = false;
    if gh.join("bin").join(gh_file(cfg!(windows))).is_file() {
        use_own_gh(&gh);
        found = true;
    }
    let git = own_git_dir(data);
    if cfg!(windows) && git.join("cmd").join("git.exe").is_file() {
        use_own_git(&git);
    }
    found
}

/// Puts `~/.local/bin` (Claude Code's own installer's folder) at the end of this process's PATH, when it's not on it.
pub fn use_local_bin(home: &Path) {
    let bin = home.join(".local").join("bin");
    let old = std::env::var_os("PATH").unwrap_or_default();
    let mut parts: Vec<PathBuf> = std::env::split_paths(&old)
        .filter(|p| !p.as_os_str().is_empty())
        .collect();
    if parts.contains(&bin) {
        return;
    }
    parts.push(bin);
    if let Ok(new) = std::env::join_paths(parts) {
        {
            use crate::secrets::Host;
            crate::secrets::ProcessHost.set("PATH", &new.to_string_lossy());
        }
    }
}

/// [`use_local_bin`] for this user's home.
pub fn use_local_bin_if_home() {
    if let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }) {
        use_local_bin(Path::new(&home));
    }
}

#[cfg(test)]
mod tests;
