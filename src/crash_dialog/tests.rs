use super::*;

#[test]
fn the_text_names_the_reason_and_the_log() {
    assert_eq!(
        crash_text(
            "its build.json couldn't be read (x).",
            Some("/l/retaskable.log"),
            "Retaskable"
        ),
        "Retaskable couldn't start: its build.json couldn't be read (x). The log is at /l/retaskable.log."
    );
    assert_eq!(
        crash_text("port taken", None, "Retaskable Staging"),
        "Retaskable Staging couldn't start: port taken."
    );
    assert_eq!(crash_text("  a b... \n", None, "R"), "R couldn't start:   a b.");
}

#[test]
fn windows_shows_a_message_box_from_the_environment_never_from_the_script() {
    let d = dialogs("it's \"bad\"", "windows", "Retaskable");
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].program, "powershell.exe");
    assert_eq!(
        &d[0].args[..5],
        [
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-EncodedCommand"
        ]
    );
    assert!(!d[0].args.iter().any(|a| a.contains("bad")));
    assert!(
        d[0].env
            .contains(&("RETASKABLE_CRASH_TEXT".to_string(), "it's \"bad\"".to_string()))
    );
    assert!(
        d[0].env
            .contains(&("RETASKABLE_CRASH_TITLE".to_string(), "Retaskable".to_string()))
    );
}

#[test]
fn the_encoded_script_is_utf16_base64() {
    // "Hi" in UTF-16LE is 48 00 69 00.
    assert_eq!(encoded("Hi"), "SABpAA==");
    assert_eq!(encoded(""), "");
    assert_eq!(encoded("a"), "YQA=");
}

#[test]
fn macos_passes_the_text_as_an_argument() {
    let d = dialogs("boom", "macos", "Retaskable");
    assert_eq!(d[0].program, "osascript");
    assert_eq!(&d[0].args[d[0].args.len() - 2..], ["boom", "Retaskable"]);
}

#[test]
fn linux_tries_zenity_then_kdialog_then_notify_send() {
    let names: Vec<_> = dialogs("boom", "linux", "Retaskable")
        .into_iter()
        .map(|d| d.program)
        .collect();
    assert_eq!(names, ["zenity", "kdialog", "notify-send"]);
    assert_eq!(
        dialogs("boom", "linux", "T")[0].args,
        ["--error", "--title=T", "--no-markup", "--text=boom"]
    );
}

#[test]
fn the_first_dialog_that_can_run_is_the_one_shown() {
    let mut tried = Vec::new();
    let shown = show_with("boom", "T", "linux", |d| {
        tried.push(d.program.clone());
        (d.program == "kdialog").then_some(())
    });
    assert_eq!(shown.as_deref(), Some("kdialog"));
    assert_eq!(tried, ["zenity", "kdialog"]);
    assert_eq!(show_with("boom", "T", "linux", |_| None), None);
}
