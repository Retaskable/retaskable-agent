//! Play on phone (docs/specs/play-on-phone.md sections 3 and 4): finding `adb`, watching for a
//! plugged-in Android phone, and the first-install warning. The button's build-and-install command
//! (section 5) and reporting to retaskable.com (section 6) are later slices; for now a found phone
//! is only logged, and nothing here is wired to a command yet.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;
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

/// One line of `adb devices -l`: `{ serial, model, state }`. Whether a serial was warned (section
/// 4) lives in `.retaskable/phones.json` (`warned_serials`), not on this struct — the same split
/// as the TypeScript agent's `Device` versus `Phone`.
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

/// Section 4's warning, before the first install to a phone (shared with the Expo app's dialog;
/// `installWarning` in `shared/phone.ts`). The work-in-progress line only shows while the app's
/// `itemId` isn't done.
pub fn install_warning(app_id: &str, wip: bool) -> Vec<String> {
    let mut lines = vec![
        "This is a debug build, signed with this computer's debug key, not the Play Store's.".to_string(),
        format!(
            "If the phone already has {app_id} signed with a different key (from the Play Store, or \
             another computer), Android won't install over it. Retaskable will offer to uninstall it \
             first, which deletes the app's data on the phone (you'll be signed out, and its \
             notification settings are lost). It never does that without asking."
        ),
        "USB debugging gives this computer broad access to the phone: it can install apps, read \
         files and run commands on it. Turn it off in the phone's Developer options when you're \
         done, and only accept the \"Allow USB debugging?\" prompt for computers you trust."
            .to_string(),
    ];
    if wip {
        lines.push("The app is work in progress: parts of it may not work yet.".to_string());
    }
    lines
}

/// When the phone already has the app signed with a different key (section 4's "A different
/// signature"), before offering **Uninstall and install**.
pub fn uninstall_warning(app_id: &str, model: &str) -> String {
    format!(
        "{model} already has {app_id} signed with a different key, so Android won't install over \
         it. Uninstalling it first deletes the app's data on the phone: you'll be signed out, and \
         its notification settings are lost."
    )
}

/// `.retaskable/phones.json`'s `warned` list (section 4): the serials that no longer need the
/// install warning. Missing, unreadable or broken reads as none — the same recovery the spec
/// names for a deleted file, never a crash. A non-string entry in the list is skipped rather than
/// failing the whole read.
pub fn warned_serials(file: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    parsed
        .get("warned")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Remembers `serial` as warned (the dialog's **Install** was confirmed), so later requests for
/// the same phone go straight to installing.
pub fn mark_warned(file: &Path, serial: &str) -> Result<()> {
    let mut warned = warned_serials(file);
    if warned.iter().any(|s| s == serial) {
        return Ok(());
    }
    warned.push(serial.to_string());
    crate::store::write_json_atomic(file, &serde_json::json!({ "warned": warned }), false)
}

/// Whether a `phone_play` request for `serial` may proceed (section 4): `serial` must be one of
/// `devices` in the `device` state, a request with `uninstall_first` must be `confirmed`, and a
/// first request for a phone not yet in `warned` is refused with the warning dialog's own text.
/// The agent checks a `phone_play` command the same way once it arrives over the cloud link
/// (section 6, not wired up yet). Returns the phone to install to, or the message to refuse with.
pub fn check_play(
    devices: &[Phone],
    warned: &[String],
    serial: &str,
    confirmed: bool,
    uninstall_first: bool,
    app_id: &str,
    wip: bool,
) -> Result<Phone, String> {
    let phone = devices
        .iter()
        .find(|d| d.serial == serial && offerable(&d.state))
        .ok_or_else(|| "That phone isn't connected with USB debugging allowed.".to_string())?;
    if uninstall_first && !confirmed {
        return Err("Uninstalling needs confirmed: true.".to_string());
    }
    if !confirmed && !warned.iter().any(|s| s == serial) {
        let mut lines = vec![format!("Install on {}?", phone.model)];
        lines.extend(install_warning(app_id, wip));
        return Err(lines.join("\n"));
    }
    Ok(phone.clone())
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
/// agent does, the same pattern as `engines::resolve_claude_on_path`.
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

    // install_warning / uninstall_warning

    #[test]
    fn install_warning_has_the_fixed_lines_plus_a_wip_line_only_while_the_app_isnt_done() {
        let done = install_warning("com.retaskable.app", false);
        assert_eq!(done.len(), 3);
        assert!(done[0].contains("debug build"));
        assert!(done[1].contains("com.retaskable.app"));
        assert!(done[2].contains("USB debugging"));

        let wip = install_warning("com.retaskable.app", true);
        assert_eq!(wip.len(), 4);
        assert!(wip[3].contains("work in progress"));
    }

    #[test]
    fn uninstall_warning_names_the_model_and_the_app_and_says_data_is_lost() {
        let text = uninstall_warning("com.retaskable.app", "Pixel 8");
        assert!(text.starts_with("Pixel 8 already has com.retaskable.app"));
        assert!(text.contains("signed out"));
    }

    // warned_serials / mark_warned

    #[test]
    fn warned_serials_is_the_files_list_or_none_when_missing_or_broken() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("phones.json");
        assert_eq!(warned_serials(&file), Vec::<String>::new());

        std::fs::write(&file, r#"{"warned": ["R58M12ABCDE", 3]}"#).unwrap();
        assert_eq!(warned_serials(&file), vec!["R58M12ABCDE".to_string()]);

        std::fs::write(&file, "{").unwrap();
        assert_eq!(warned_serials(&file), Vec::<String>::new());
    }

    #[test]
    fn mark_warned_adds_the_serial_once_and_keeps_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("phones.json");

        mark_warned(&file, "AAA").unwrap();
        assert_eq!(warned_serials(&file), vec!["AAA".to_string()]);

        mark_warned(&file, "BBB").unwrap();
        assert_eq!(warned_serials(&file), vec!["AAA".to_string(), "BBB".to_string()]);

        // Already warned: no duplicate.
        mark_warned(&file, "AAA").unwrap();
        assert_eq!(warned_serials(&file), vec!["AAA".to_string(), "BBB".to_string()]);
    }

    // check_play

    fn a_phone() -> Phone {
        Phone {
            serial: "AAA".to_string(),
            model: "Pixel 8".to_string(),
            state: "device".to_string(),
        }
    }

    #[test]
    fn check_play_refuses_a_serial_that_isnt_a_currently_authorised_phone() {
        let err = check_play(&[], &[], "AAA", true, false, "com.retaskable.app", false).unwrap_err();
        assert_eq!(err, "That phone isn't connected with USB debugging allowed.");

        let unauthorized = Phone {
            state: "unauthorized".to_string(),
            ..a_phone()
        };
        let err = check_play(&[unauthorized], &[], "AAA", true, false, "com.retaskable.app", false).unwrap_err();
        assert_eq!(err, "That phone isn't connected with USB debugging allowed.");
    }

    #[test]
    fn check_play_refuses_an_unconfirmed_uninstall() {
        let err = check_play(&[a_phone()], &[], "AAA", false, true, "com.retaskable.app", false).unwrap_err();
        assert_eq!(err, "Uninstalling needs confirmed: true.");
    }

    #[test]
    fn check_play_refuses_an_unconfirmed_first_install_with_the_warnings_text() {
        let err = check_play(&[a_phone()], &[], "AAA", false, false, "com.retaskable.app", true).unwrap_err();
        let mut expected = vec!["Install on Pixel 8?".to_string()];
        expected.extend(install_warning("com.retaskable.app", true));
        assert_eq!(err, expected.join("\n"));
    }

    #[test]
    fn check_play_allows_a_confirmed_request() {
        let phone = check_play(&[a_phone()], &[], "AAA", true, false, "com.retaskable.app", false).unwrap();
        assert_eq!(phone, a_phone());
    }

    #[test]
    fn check_play_allows_an_unconfirmed_request_for_an_already_warned_phone() {
        let phone = check_play(
            &[a_phone()],
            &["AAA".to_string()],
            "AAA",
            false,
            false,
            "com.retaskable.app",
            false,
        )
        .unwrap();
        assert_eq!(phone, a_phone());
    }
}
