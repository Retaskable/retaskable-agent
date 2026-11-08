//! A native notification for the tray (tray/04-the-tray-agent.md section 4.6): the OS's own tool, run without a
//! shell. Best effort: a missing tool logs at debug and does nothing. Only called while the tray is up.

use std::process::{Command, Stdio};

pub const TITLE: &str = "Retaskable: waiting for you";
const TITLE_VAR: &str = "RETASKABLE_NOTIFY_TITLE";
const TEXT_VAR: &str = "RETASKABLE_NOTIFY_TEXT";
/// PowerShell's own app id, so the toast shows without registering one.
const POWERSHELL_SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null
$xml = New-Object Windows.Data.Xml.Dom.XmlDocument
$xml.LoadXml('<toast><visual><binding template="ToastText02"><text id="1"></text><text id="2"></text></binding></visual></toast>')
$texts = $xml.GetElementsByTagName('text')
$texts.Item(0).AppendChild($xml.CreateTextNode($env:RETASKABLE_NOTIFY_TITLE)) | Out-Null
$texts.Item(1).AppendChild($xml.CreateTextNode($env:RETASKABLE_NOTIFY_TEXT)) | Out-Null
$toast = New-Object Windows.UI.Notifications.ToastNotification $xml
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe').Show($toast)
"#;

pub type NotifyCommand = (String, Vec<String>, Vec<(String, String)>);

/// The command for `os` (a `std::env::consts::OS` value): program, arguments, extra environment.
pub fn notify_command(os: &str, title: &str, text: &str) -> NotifyCommand {
    let s = String::from;
    match os {
        "windows" => {
            use base64::Engine;
            let utf16: Vec<u8> = POWERSHELL_SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
            (
                s("powershell.exe"),
                ["-NoProfile", "-NonInteractive", "-EncodedCommand"]
                    .map(s)
                    .into_iter()
                    .chain([encoded])
                    .collect(),
                vec![(s(TITLE_VAR), s(title)), (s(TEXT_VAR), s(text))],
            )
        }
        "macos" => (
            s("osascript"),
            vec![
                s("-e"),
                s("on run argv"),
                s("-e"),
                s("display notification (item 1 of argv) with title (item 2 of argv)"),
                s("-e"),
                s("end run"),
                s(text),
                s(title),
            ],
            vec![],
        ),
        _ => (s("notify-send"), vec![s("--"), s(title), s(text)], vec![]),
    }
}

fn spawn(command: &NotifyCommand) -> std::io::Result<()> {
    let (program, args, env) = command;
    let mut cmd = Command::new(program);
    cmd.args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Show `text` under [`TITLE`]. Never fails: a missing tool is logged at debug.
pub fn notify(text: &str) {
    let command = notify_command(std::env::consts::OS, TITLE, text);
    if let Err(e) = spawn(&command) {
        tracing::debug!("no native notification ({}): {e}", command.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn windows_runs_a_fixed_encoded_script_with_the_text_in_env() {
        let (program, args, env) = notify_command("windows", "T \"x\"", "it's $(evil)");
        assert_eq!(program, "powershell.exe");
        assert_eq!(args[..3], ["-NoProfile", "-NonInteractive", "-EncodedCommand"]);
        let bytes = base64::engine::general_purpose::STANDARD.decode(&args[3]).unwrap();
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let script = String::from_utf16(&units).unwrap();
        assert_eq!(script, POWERSHELL_SCRIPT);
        assert!(!script.contains("evil") && !script.contains("it's"));
        assert!(!args.iter().any(|a| a.contains("evil")));
        assert_eq!(
            env,
            vec![
                (TITLE_VAR.to_string(), "T \"x\"".to_string()),
                (TEXT_VAR.to_string(), "it's $(evil)".to_string())
            ]
        );
    }

    #[test]
    fn macos_passes_the_text_as_script_arguments() {
        let (program, args, env) = notify_command("macos", TITLE, "approve it");
        assert_eq!(program, "osascript");
        assert_eq!(args[args.len() - 2..], ["approve it", TITLE]);
        assert!(args[..args.len() - 2].iter().all(|a| !a.contains("approve it")));
        assert!(env.is_empty());
    }

    #[test]
    fn linux_uses_notify_send() {
        let (program, args, env) = notify_command("linux", TITLE, "approve it");
        assert_eq!(program, "notify-send");
        assert_eq!(args, ["--", TITLE, "approve it"]);
        assert!(env.is_empty());
    }

    #[test]
    fn a_missing_program_is_not_an_error() {
        let missing = ("retaskable-no-such-tool".to_string(), vec![], vec![]);
        assert!(spawn(&missing).is_err());
    }
}
