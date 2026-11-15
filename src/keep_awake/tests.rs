use super::*;

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("keep-awake-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn each_os_has_its_own_command_for_the_agents_pid() {
    let (p, a) = awake_command("macos", 42).unwrap();
    assert_eq!((p.as_str(), a.join(" ")), ("caffeinate", "-i -s -w 42".to_string()));
    let (p, a) = awake_command("linux", 42).unwrap();
    assert_eq!(p, "systemd-inhibit");
    assert!(a.contains(&"--what=idle:sleep".to_string()) && a.contains(&"tail".to_string()));
    assert_eq!(a.last().unwrap(), "42");
    let (p, a) = awake_command("windows", 42).unwrap();
    assert_eq!(p, "powershell");
    let script = a.last().unwrap();
    assert!(script.contains("SetThreadExecutionState") && script.contains("Wait-Process -Id 42"));
    assert!(awake_command("freebsd", 1).is_none());
}

#[test]
fn the_choice_is_a_file_that_a_new_instance_finds_again() {
    let dir = temp("choice");
    let file = dir.join(CHOICE_FILE);
    let on = KeepAwake::with_command(file.clone(), None);
    on.set(true).unwrap();
    // No tool to run here: it is left off, as for a missing tool.
    assert!(!on.is_on());
    std::fs::write(&file, "on").unwrap();
    assert!(KeepAwake::with_command(file.clone(), None).is_on());
    KeepAwake::with_command(file.clone(), None).set(false).unwrap();
    assert!(!file.exists());
}

#[test]
fn a_missing_tool_leaves_it_off() {
    let dir = temp("missing");
    let k = KeepAwake::with_command(dir.join(CHOICE_FILE), Some(("no-such-tool-retaskable".into(), vec![])));
    k.set(true).unwrap();
    assert!(!k.is_on());
}

#[cfg(unix)]
#[test]
fn a_running_tool_holds_it_on_stopping_keeps_the_choice_and_an_ended_tool_clears_it() {
    let dir = temp("run");
    let file = dir.join(CHOICE_FILE);
    let k = KeepAwake::with_command(file.clone(), Some(("sleep".into(), vec!["30".into()])));
    k.set(true).unwrap();
    assert!(k.is_on());
    k.stop();
    assert!(k.is_on(), "stopping the agent keeps the choice");
    k.tick();
    assert!(k.is_on());
    k.set(false).unwrap();
    assert!(!k.is_on());
    let short = KeepAwake::with_command(file, Some(("true".into(), vec![])));
    short.set(true).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    short.tick();
    assert!(!short.is_on(), "a tool that ends by itself leaves it off");
}
