//! Play on phone (docs/specs/play-on-phone.md section 3): finding `adb` and watching for a
//! plugged-in Android phone. This is slice 1 of the play-on-phone item — the warning, the button,
//! the install command and reporting to retaskable.com (sections 4 to 6) are later slices; for now
//! a found phone is only logged.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

/// "adb devices -l" every 5 seconds (section 3), outside any sandbox.
pub const POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Each poll's own timeout, so a stuck adb server never stalls the next one.
pub const POLL_TIMEOUT: Duration = Duration::from_secs(10);

/// `adb wasn't found` (section 3's "When something's missing"), shown in place of `phone.status`
/// once the site reports it (a later slice).
pub const NO_ADB: &str =
    "adb wasn't found. Install Android's SDK Platform-Tools and set ANDROID_HOME, or put adb on your PATH.";

/// One line of `adb devices -l`: `{ serial, model, state }`. `warned` (section 4, whether the
/// install warning was already accepted for this serial) isn't tracked yet — that's the slice
/// that adds the warning dialog.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phone {
    pub serial: String,
    pub model: String,
    pub state: String,
}

/// Only a phone in the `device` state (adb's name for one that's authorised and connected) can be
/// installed to. `unauthorized`, `offline`, `recovery`, `sideload`, `bootloader` and anything else
/// are never offered (section 3). Emulators (`emulator-*` serials) count like phones: adb reports
/// them the same way, so nothing here treats them differently.
pub fn offerable(state: &str) -> bool {
    state == "device"
}

/// Parses `adb devices -l`'s output into its device lines, skipping the header and any blank
/// line. The model comes from the `model:` field, underscores as spaces, or the serial when
/// there's none.
pub fn parse_devices(output: &str) -> Vec<Phone> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "List of devices attached")
        .filter_map(parse_line)
        .collect()
}

fn parse_line(line: &str) -> Option<Phone> {
    let mut fields = line.split_whitespace();
    let serial = fields.next()?.to_string();
    let state = fields.next()?.to_string();
    let model = fields
        .find_map(|f| f.strip_prefix("model:"))
        .map(|m| m.replace('_', " "))
        .unwrap_or_else(|| serial.clone());
    Some(Phone { serial, model, state })
}

/// Finding adb, in order (section 3): `$ANDROID_HOME/platform-tools/adb`, then
/// `$ANDROID_SDK_ROOT/platform-tools/adb`, then `adb` on `PATH`, then `default_sdk`'s
/// `platform-tools/adb` (the SDK Android Studio installs by default: `%LOCALAPPDATA%\Android\Sdk`
/// on Windows, `~/Library/Android/sdk` on macOS, `~/Android/Sdk` on Linux). Only a path that
/// exists is returned; the caller's `exe_name` carries the platform's extension (`adb.exe` on
/// Windows).
pub fn find_adb(
    env: impl Fn(&str) -> Option<String>,
    exe_name: &str,
    path_dirs: &[PathBuf],
    default_sdk: Option<&Path>,
) -> Option<PathBuf> {
    for sdk in [env("ANDROID_HOME"), env("ANDROID_SDK_ROOT")].into_iter().flatten() {
        let candidate = Path::new(&sdk).join("platform-tools").join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    for dir in path_dirs {
        let candidate = dir.join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    if let Some(sdk) = default_sdk {
        let candidate = sdk.join("platform-tools").join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// `find_adb`, reading `PATH`, `LOCALAPPDATA` and the home directory itself the way the real
/// agent does, the same pattern as `session::resolve_claude_on_path`.
pub fn find_adb_on_path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let windows = cfg!(windows);
    let exe_name = if windows { "adb.exe" } else { "adb" };
    let path = env("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let default_sdk = default_sdk_dir(&env, windows);
    find_adb(env, exe_name, &dirs, default_sdk.as_deref())
}

fn default_sdk_dir(env: &impl Fn(&str) -> Option<String>, windows: bool) -> Option<PathBuf> {
    if windows {
        return env("LOCALAPPDATA").map(|p| Path::new(&p).join("Android").join("Sdk"));
    }
    let home = env("HOME")?;
    let home = Path::new(&home);
    Some(if cfg!(target_os = "macos") {
        home.join("Library").join("Android").join("sdk")
    } else {
        home.join("Android").join("Sdk")
    })
}

/// Runs `adb devices -l` once, never through a shell, killed if it outlasts `POLL_TIMEOUT`.
async fn run_devices_l(adb: &Path) -> Result<String, String> {
    let mut cmd = Command::new(adb);
    cmd.args(["devices", "-l"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    let output = match timeout(POLL_TIMEOUT, child.wait_with_output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(e.to_string()),
        Err(_) => return Err("adb devices timed out".to_string()),
    };
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        return Err(err.lines().next().unwrap_or("adb devices failed").trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// One poll: the phones currently plugged in and authorised to see, or the error to show in
/// `phone.status` (retried at the next poll, section 3).
pub async fn list_devices(adb: &Path) -> Result<Vec<Phone>, String> {
    run_devices_l(adb).await.map(|out| parse_devices(&out))
}

/// Watches for phones every `POLL_INTERVAL`, logging the plugged-in, authorised phones when the
/// list changes, and never while the previous poll is still running (each poll is awaited before
/// the next is scheduled). Runs until the process exits; wiring this up to the cloud link and
/// `phone.phones` belongs to a later slice.
pub async fn watch(adb: PathBuf) -> ! {
    let mut last: Option<Result<Vec<Phone>, String>> = None;
    loop {
        let result = list_devices(&adb).await;
        if last.as_ref() != Some(&result) {
            match &result {
                Ok(phones) if phones.is_empty() => tracing::info!("Play on phone: no phone plugged in"),
                Ok(phones) => {
                    let desc = phones
                        .iter()
                        .map(|p| format!("{} ({}, {})", p.model, p.serial, p.state))
                        .collect::<Vec<_>>()
                        .join(", ");
                    tracing::info!("Play on phone: {desc}");
                }
                Err(e) => tracing::warn!("Play on phone: {e}"),
            }
            last = Some(result);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // offerable

    #[test]
    fn only_the_device_state_is_offerable() {
        assert!(offerable("device"));
        for state in [
            "unauthorized",
            "offline",
            "recovery",
            "sideload",
            "bootloader",
            "anything-else",
        ] {
            assert!(!offerable(state), "{state} should not be offerable");
        }
    }

    // parse_devices

    #[test]
    fn parse_devices_reads_serial_state_and_model() {
        let output = "List of devices attached\n\
                       0A1B2C3D       device usb:1-1 product:product_name model:Pixel_8 device:raven transport_id:1\n";
        assert_eq!(
            parse_devices(output),
            vec![Phone {
                serial: "0A1B2C3D".to_string(),
                model: "Pixel 8".to_string(),
                state: "device".to_string(),
            }]
        );
    }

    #[test]
    fn parse_devices_falls_back_to_the_serial_when_there_is_no_model_field() {
        let output = "List of devices attached\nHT12345              unauthorized usb:1-2 transport_id:3\n";
        assert_eq!(
            parse_devices(output),
            vec![Phone {
                serial: "HT12345".to_string(),
                model: "HT12345".to_string(),
                state: "unauthorized".to_string(),
            }]
        );
    }

    #[test]
    fn parse_devices_skips_the_header_and_blank_lines() {
        let output = "List of devices attached\n\n\n";
        assert_eq!(parse_devices(output), vec![]);
        assert_eq!(parse_devices(""), vec![]);
    }

    #[test]
    fn parse_devices_reads_an_emulator_serial_like_any_other_phone() {
        let output = "List of devices attached\nemulator-5554 device product:sdk_gphone64_arm64 model:sdk_gphone64_arm64 device:emulator64_arm64 transport_id:2\n";
        let phones = parse_devices(output);
        assert_eq!(phones.len(), 1);
        assert_eq!(phones[0].serial, "emulator-5554");
        assert!(offerable(&phones[0].state));
    }

    #[test]
    fn parse_devices_reads_several_lines() {
        let output = "List of devices attached\n\
                       AAA device model:Pixel_8\n\
                       BBB offline model:Pixel_6\n";
        let phones = parse_devices(output);
        assert_eq!(phones.len(), 2);
        assert_eq!(phones[0].model, "Pixel 8");
        assert_eq!(phones[1].state, "offline");
    }

    // find_adb

    #[test]
    fn find_adb_prefers_android_home() {
        let dir = tempfile::tempdir().unwrap();
        let platform_tools = dir.path().join("platform-tools");
        std::fs::create_dir_all(&platform_tools).unwrap();
        std::fs::write(platform_tools.join("adb"), "").unwrap();
        let home = dir.path().to_string_lossy().to_string();
        let env = move |k: &str| (k == "ANDROID_HOME").then(|| home.clone());

        assert_eq!(find_adb(env, "adb", &[], None), Some(platform_tools.join("adb")));
    }

    #[test]
    fn find_adb_falls_back_to_android_sdk_root_then_path_then_the_default_sdk() {
        fn no_env(_: &str) -> Option<String> {
            None
        }

        // Nothing anywhere: not found.
        assert_eq!(find_adb(no_env, "adb", &[], None), None);

        // On PATH.
        let path_dir = tempfile::tempdir().unwrap();
        std::fs::write(path_dir.path().join("adb"), "").unwrap();
        assert_eq!(
            find_adb(no_env, "adb", &[path_dir.path().to_path_buf()], None),
            Some(path_dir.path().join("adb"))
        );

        // Only in the default SDK location.
        let sdk_dir = tempfile::tempdir().unwrap();
        let platform_tools = sdk_dir.path().join("platform-tools");
        std::fs::create_dir_all(&platform_tools).unwrap();
        std::fs::write(platform_tools.join("adb"), "").unwrap();
        assert_eq!(
            find_adb(no_env, "adb", &[], Some(sdk_dir.path())),
            Some(platform_tools.join("adb"))
        );
    }

    #[test]
    fn find_adb_on_path_checks_android_sdk_root_when_android_home_is_unset() {
        let dir = tempfile::tempdir().unwrap();
        let platform_tools = dir.path().join("platform-tools");
        std::fs::create_dir_all(&platform_tools).unwrap();
        let exe_name = if cfg!(windows) { "adb.exe" } else { "adb" };
        std::fs::write(platform_tools.join(exe_name), "").unwrap();
        let sdk_root = dir.path().to_string_lossy().to_string();
        let env = move |k: &str| (k == "ANDROID_SDK_ROOT").then(|| sdk_root.clone());

        assert_eq!(find_adb_on_path(env), Some(platform_tools.join(exe_name)));
    }
}
