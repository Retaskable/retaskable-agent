//! A native notification for the tray (tray/04-the-tray-agent.md section 4.6): the OS's own tool, run without a
//! shell. Best effort: a missing tool logs at debug and does nothing. Only called while the tray is up.

use std::process::Stdio;

pub const TITLE: &str = "Retaskable: waiting for you";
const TITLE_VAR: &str = "RETASKABLE_NOTIFY_TITLE";
const BODY_VAR: &str = "RETASKABLE_NOTIFY_BODY";
/// Windows PowerShell 5.1's toast, under PowerShell's own app id (a toast needs a registered one). The text is read from
/// the environment, so nothing in it is ever parsed as PowerShell. Buttons: each opens its link; the toast stays until
/// one is pressed.
const POWERSHELL_SCRIPT: &str = r#"$ErrorActionPreference = 'Stop'
[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
[Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null
$xml = New-Object Windows.Data.Xml.Dom.XmlDocument
$xml.LoadXml('<toast><visual><binding template="ToastGeneric"><text/><text/></binding></visual></toast>')
$text = $xml.GetElementsByTagName('text')
$text.Item(0).AppendChild($xml.CreateTextNode($env:RETASKABLE_NOTIFY_TITLE)) | Out-Null
$text.Item(1).AppendChild($xml.CreateTextNode($env:RETASKABLE_NOTIFY_BODY)) | Out-Null
$buttons = @()
if ($env:RETASKABLE_NOTIFY_BUTTON1 -and $env:RETASKABLE_NOTIFY_LINK1) { $buttons += ,@($env:RETASKABLE_NOTIFY_BUTTON1, $env:RETASKABLE_NOTIFY_LINK1) }
if ($env:RETASKABLE_NOTIFY_BUTTON2 -and $env:RETASKABLE_NOTIFY_LINK2) { $buttons += ,@($env:RETASKABLE_NOTIFY_BUTTON2, $env:RETASKABLE_NOTIFY_LINK2) }
if ($buttons.Count) {
  $xml.DocumentElement.SetAttribute('scenario', 'reminder')
  $actions = $xml.CreateElement('actions')
  foreach ($b in $buttons) { $a = $xml.CreateElement('action'); $a.SetAttribute('content', $b[0]); $a.SetAttribute('arguments', $b[1]); $a.SetAttribute('activationType', 'protocol'); $actions.AppendChild($a) | Out-Null }
  $xml.DocumentElement.AppendChild($actions) | Out-Null
}
$app = '{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe'
[Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier($app).Show([Windows.UI.Notifications.ToastNotification]::new($xml))
"#;

/// A notification's button: its label and the link it opens (Windows only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Button {
    pub label: String,
    pub link: String,
}

pub type NotifyCommand = (String, Vec<String>, Vec<(String, String)>);

/// The command for `os` (a `std::env::consts::OS` value): program, arguments, extra environment. Only the first two
/// buttons are used.
pub fn notify_command(os: &str, title: &str, body: &str, buttons: &[Button]) -> NotifyCommand {
    let s = String::from;
    match os {
        "windows" => {
            use base64::Engine;
            let utf16: Vec<u8> = POWERSHELL_SCRIPT.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let encoded = base64::engine::general_purpose::STANDARD.encode(utf16);
            let mut env = vec![(s(TITLE_VAR), s(title)), (s(BODY_VAR), s(body))];
            for (i, b) in buttons.iter().take(2).enumerate() {
                env.push((format!("RETASKABLE_NOTIFY_BUTTON{}", i + 1), b.label.clone()));
                env.push((format!("RETASKABLE_NOTIFY_LINK{}", i + 1), b.link.clone()));
            }
            (
                s("powershell.exe"),
                [
                    "-NoProfile",
                    "-NonInteractive",
                    "-WindowStyle",
                    "Hidden",
                    "-EncodedCommand",
                ]
                .map(s)
                .into_iter()
                .chain([encoded])
                .collect(),
                env,
            )
        }
        "macos" => (
            s("osascript"),
            vec![
                s("-e"),
                s("on run argv"),
                s("-e"),
                s("display notification (item 2 of argv) with title (item 1 of argv)"),
                s("-e"),
                s("end run"),
                s(title),
                s(body),
            ],
            vec![],
        ),
        _ => (
            s("notify-send"),
            vec![s("--app-name=Retaskable"), s(title), s(body)],
            vec![],
        ),
    }
}

fn spawn(command: &NotifyCommand) -> std::io::Result<()> {
    let (program, args, env) = command;
    let mut cmd = crate::spawn::command(program);
    cmd.args(args)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Shows a notification with a title and a line of text. Best effort: where the OS's tool is missing (a Linux without
/// notify-send) or fails, nothing is shown and the tray menu still has it.
pub fn notify_titled(title: &str, body: &str, buttons: &[Button]) {
    let command = notify_command(std::env::consts::OS, title, body, buttons);
    if let Err(e) = spawn(&command) {
        tracing::debug!("no native notification ({}): {e}", command.0);
    }
}

/// Show `text` under [`TITLE`]. Never fails: a missing tool is logged at debug.
pub fn notify(text: &str) {
    notify_titled(TITLE, text, &[]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    #[test]
    fn windows_runs_a_fixed_encoded_script_with_the_text_in_env() {
        let (program, args, env) = notify_command("windows", "T \"x\"", "it's $(evil)", &[]);
        assert_eq!(program, "powershell.exe");
        assert_eq!(args[..4], ["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden"]);
        let bytes = base64::engine::general_purpose::STANDARD.decode(&args[5]).unwrap();
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let script = String::from_utf16(&units).unwrap();
        assert_eq!(script, POWERSHELL_SCRIPT);
        assert!(!script.contains("evil") && !script.contains("it's"));
        assert!(!args.iter().any(|a| a.contains("evil")));
        assert_eq!(
            env,
            vec![
                (TITLE_VAR.to_string(), "T \"x\"".to_string()),
                (BODY_VAR.to_string(), "it's $(evil)".to_string())
            ]
        );
    }

    #[test]
    fn macos_passes_the_text_as_script_arguments() {
        let (program, args, env) = notify_command("macos", TITLE, "approve it", &[]);
        assert_eq!(program, "osascript");
        assert_eq!(args[args.len() - 2..], [TITLE, "approve it"]);
        assert!(args[..args.len() - 2].iter().all(|a| !a.contains("approve it")));
        assert!(env.is_empty());
    }

    #[test]
    fn linux_uses_notify_send() {
        let (program, args, env) = notify_command("linux", TITLE, "approve it", &[]);
        assert_eq!(program, "notify-send");
        assert_eq!(args, ["--app-name=Retaskable", TITLE, "approve it"]);
        assert!(env.is_empty());
    }

    #[test]
    fn a_title_of_its_own_and_buttons_go_in_the_environment_two_at_most() {
        let buttons: Vec<Button> = (1..=3)
            .map(|n| Button {
                label: format!("Open {n}"),
                link: format!("https://x/{n}"),
            })
            .collect();
        let (_, args, env) = notify_command("windows", "CI is red on main", "Fix it", &buttons);
        assert!(!args.iter().any(|a| a.contains("Open")));
        let get = |k: &str| env.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str());
        assert_eq!(get(TITLE_VAR), Some("CI is red on main"));
        assert_eq!(get("RETASKABLE_NOTIFY_BUTTON2"), Some("Open 2"));
        assert_eq!(get("RETASKABLE_NOTIFY_LINK2"), Some("https://x/2"));
        assert_eq!(get("RETASKABLE_NOTIFY_BUTTON3"), None);
        // Elsewhere the title is the notification's own and there are no buttons.
        let (_, args, env) = notify_command("linux", "CI is red on main", "Fix it", &buttons);
        assert_eq!(args, ["--app-name=Retaskable", "CI is red on main", "Fix it"]);
        assert!(env.is_empty());
    }

    #[test]
    fn a_missing_program_is_not_an_error() {
        let missing = ("retaskable-no-such-tool".to_string(), vec![], vec![]);
        assert!(spawn(&missing).is_err());
    }
}
