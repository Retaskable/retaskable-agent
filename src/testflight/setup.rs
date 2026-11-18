//! What a build needs of the project and the computer, and the two pure edits it makes (`setupRefusal`,
//! `testflightReady`, `versionUrlOf`, `buildNumberAt`, `pinBuildNumber` in `agent/src/testflight.ts`).
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;

use super::Environment;
use crate::config::ProjectConfig;

/// The build command, fixed (the `build:ios:testflight` script, an Xcode archive and upload), run from `app/`.
pub fn build_script() -> PathBuf {
    Path::new("scripts").join("testflight-ios.sh")
}

/// The Xcode project the script archives, and where its build number lives, under `app/`.
pub fn xcodeproj() -> PathBuf {
    Path::new("ios").join("Retaskable.xcodeproj").join("project.pbxproj")
}

/// `<repo>/app`.
pub(super) fn app_dir(repo: &Path) -> PathBuf {
    repo.join("app")
}

/// Why this computer can't archive the app with Xcode, or `None`: a Mac, with the committed iOS project and the script.
pub fn setup_refusal(project: &ProjectConfig, mac: bool) -> Option<&'static str> {
    if !mac {
        return Some("TestFlight builds need a Mac: Xcode archives the app.");
    }
    let app = app_dir(Path::new(&project.repo_path));
    (!app.join(xcodeproj()).exists() || !app.join(build_script()).exists())
        .then_some("This project has no app/ios Xcode project and app/scripts/testflight-ios.sh.")
}

/// The setting is on and the computer can archive with Xcode: what a request must find to be claimed here.
pub fn ready(project: &ProjectConfig, mac: bool) -> bool {
    project.release.testflight && setup_refusal(project, mac).is_none()
}

/// An environment's version URL (its address plus `versionPath`, default `/version.json`), or `None` with no address.
pub fn version_url_of(project: &ProjectConfig, environment: Environment) -> Option<String> {
    let target = match environment {
        Environment::Staging => project.deploy.staging.as_ref(),
        Environment::Production => project.deploy.production.as_ref(),
    }?;
    let url = target.url.as_deref().map(str::trim).filter(|u| !u.is_empty())?;
    Some(crate::promote::site::version_url(url, target.version_path.as_deref()))
}

/// A UTC timestamp, `YYMMDDHHmm`: higher than the last build's.
pub fn build_number_at(ms: u64) -> String {
    let at = chrono::DateTime::from_timestamp_millis(ms as i64).unwrap_or_default();
    at.format("%y%m%d%H%M").to_string()
}

/// App Store Connect refuses a build number it has seen, and the number lives in the committed Xcode project: puts the
/// build's own (higher than the last) in the throwaway checkout's copy, every `CURRENT_PROJECT_VERSION = ...;`.
pub fn pin_build_number(app_dir: &Path, build_number: &str) -> std::io::Result<()> {
    static VERSION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"CURRENT_PROJECT_VERSION = [^;]+;").unwrap());
    let file = app_dir.join(xcodeproj());
    let text = std::fs::read_to_string(&file)?;
    let pinned = VERSION.replace_all(
        &text,
        regex::NoExpand(&format!("CURRENT_PROJECT_VERSION = {build_number};")),
    );
    std::fs::write(&file, pinned.as_bytes())
}
