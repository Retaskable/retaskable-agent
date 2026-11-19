//! A crash before the tray is up still has to be seen (installers/03 section 3.2): a release build started from a launcher
//! has no terminal, so it shows a dialog, "Retaskable couldn't start: <reason>. The log is at <path>.", in the OS's own
//! way: a message box on Windows, `osascript`'s `display dialog` on macOS, `zenity` or `kdialog` on Linux when either is
//! there, else `notify-send`. The text goes in as an argument (on Windows, in the environment), never as part of a
//! script. Ported from `agent/src/release.ts` (`crashText`, `crashDialogs`, `showCrashDialog`).
use std::ffi::OsString;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How long a dialog is waited for (it blocks until closed).
const WAIT: Duration = Duration::from_secs(10 * 60);

/// Set once the tray is up: a crash after that is seen as the icon going, and in the log.
static UP: AtomicBool = AtomicBool::new(false);

pub fn mark_up() {
    UP.store(true, Ordering::SeqCst);
}

pub fn is_up() -> bool {
    UP.load(Ordering::SeqCst)
}

/// "Retaskable couldn't start: <reason>. The log is at <path>.", without the log when there's none yet.
pub fn crash_text(reason: &str, log_file: Option<&str>, name: &str) -> String {
    let reason = reason.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
    let log = log_file.map(|f| format!(" The log is at {f}.")).unwrap_or_default();
    format!("{name} couldn't start: {reason}.{log}")
}

/// One dialog program to try.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dialog {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

fn dialog(program: &str, args: &[&str]) -> Dialog {
    Dialog {
        program: program.into(),
        args: args.iter().map(std::string::ToString::to_string).collect(),
        env: Vec::new(),
    }
}

/// UTF-16LE then base64, which PowerShell's `-EncodedCommand` takes (so no quoting can reach the script).
fn encoded(script: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// The OS's own dialogs to show `text` in, best first; on Linux whichever of them is installed. `os` is
/// `std::env::consts::OS`.
pub fn dialogs(text: &str, os: &str, title: &str) -> Vec<Dialog> {
    match os {
        "windows" => {
            let script = "Add-Type -AssemblyName PresentationFramework; [System.Windows.MessageBox]::Show($env:RETASKABLE_CRASH_TEXT, $env:RETASKABLE_CRASH_TITLE, 'OK', 'Error') | Out-Null";
            vec![Dialog {
                program: "powershell.exe".into(),
                args: [
                    "-NoProfile",
                    "-NonInteractive",
                    "-WindowStyle",
                    "Hidden",
                    "-EncodedCommand",
                ]
                .iter()
                .map(std::string::ToString::to_string)
                .chain([encoded(script)])
                .collect(),
                env: vec![
                    ("RETASKABLE_CRASH_TEXT".into(), text.into()),
                    ("RETASKABLE_CRASH_TITLE".into(), title.into()),
                ],
            }]
        }
        "macos" => vec![dialog(
            "osascript",
            &[
                "-e",
                "on run argv",
                "-e",
                "display dialog (item 1 of argv) with title (item 2 of argv) buttons {\"OK\"} default button 1 with icon stop",
                "-e",
                "end run",
                text,
                title,
            ],
        )],
        _ => vec![
            dialog(
                "zenity",
                &[
                    "--error",
                    &format!("--title={title}"),
                    "--no-markup",
                    &format!("--text={text}"),
                ],
            ),
            dialog("kdialog", &["--title", title, "--error", text]),
            dialog(
                "notify-send",
                &[&format!("--app-name={title}"), "--urgency=critical", title, text],
            ),
        ],
    }
}

/// A failure before the tray is up, in a release build: shows [`crash_text`] with the reason and where the log is.
/// A checkout (no `build.json`) has a terminal for it, and a failure after the tray is up is seen as the icon going.
pub fn on_failure(reason: &str) {
    if is_up() {
        return;
    }
    let env = crate::env::Env::from_process();
    let Some(Ok(Some(build))) = std::env::current_exe().ok().map(|e| crate::release::load_build(&e)) else {
        return;
    };
    let log = crate::release::logs_dir(&build, &env).join("retaskable.log");
    let name = crate::release::release_name(build.environment);
    show_text(&crash_text(reason, Some(&log.to_string_lossy()), name), name);
}

/// [`show`], for a caller with no use for which dialog it was.
pub fn show_text(text: &str, title: &str) {
    let _ = show(text, title);
}

/// Shows `text` in the first of [`dialogs`] that runs, and waits for it to be closed (up to 10 minutes). The one shown, or
/// `None` when none could run.
pub fn show(text: &str, title: &str) -> Option<String> {
    show_with(text, title, std::env::consts::OS, |d| {
        let mut cmd = crate::spawn::command(&d.program);
        cmd.args(&d.args)
            .envs(d.env.iter().map(|(k, v)| (OsString::from(k), OsString::from(v))))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let mut child = cmd.spawn().ok()?;
        // Waits for it to be closed, up to ten minutes.
        let start = std::time::Instant::now();
        while child.try_wait().ok()?.is_none() {
            if start.elapsed() >= WAIT {
                let _ = child.kill();
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        Some(())
    })
}

fn show_with(text: &str, title: &str, os: &str, mut run: impl FnMut(&Dialog) -> Option<()>) -> Option<String> {
    dialogs(text, os, title)
        .into_iter()
        .find_map(|d| run(&d).map(|()| d.program))
}

#[cfg(test)]
mod tests;
