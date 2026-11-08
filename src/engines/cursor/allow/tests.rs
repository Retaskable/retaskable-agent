use super::*;

fn check(command: &str) -> CheckConfig {
    CheckConfig {
        command: command.into(),
        name: String::new(),
        blocking: false,
        timeout_sec: 0,
    }
}

fn gallery(command: &str) -> GalleryConfig {
    GalleryConfig {
        command: command.into(),
        name: String::new(),
        out_dir: String::new(),
    }
}

#[test]
fn program_skips_assignments_and_quotes() {
    assert_eq!(command_program("npm run typecheck").as_deref(), Some("npm"));
    assert_eq!(command_program("CI=1 cargo test").as_deref(), Some("cargo"));
    assert_eq!(command_program("\"./gradlew\" build").as_deref(), Some("./gradlew"));
    assert_eq!(
        command_program("cd shots && npx playwright test").as_deref(),
        Some("cd")
    );
    assert_eq!(command_program(""), None);
}

#[test]
fn shells_once_each_in_order_without_denied_or_launchers() {
    let checks = [
        check("npm run typecheck"),
        check("CI=1 cargo test"),
        check("bash scripts/ci.sh"),
        check("npm test"),
        check(""),
    ];
    let galleries = [
        gallery("/bin/bash x"),
        gallery("bash.exe x"),
        gallery("PowerShell -f x"),
        gallery("rm -rf x"),
        gallery("make shots"),
    ];
    assert_eq!(
        command_shells(&checks, &galleries),
        ["Shell(npm)", "Shell(cargo)", "Shell(make)"]
    );
}

#[test]
fn refusals_have_their_reasons() {
    let every = |e: &str| Some(format!("{e} would allow every command"));
    assert_eq!(
        allow_entry_problem("Shell(*)").as_deref(),
        Some("Shell(*) would allow every command")
    );
    assert_eq!(allow_entry_problem("Shell(/bin/bash)"), every("Shell(/bin/bash)"));
    assert_eq!(allow_entry_problem("Shell(PowerShell)"), every("Shell(PowerShell)"));
    for w in ["Write(docs/../../etc)", "Write(/etc)", "Write(C:\\x)"] {
        assert_eq!(
            allow_entry_problem(w),
            Some(format!("{w} writes outside the workspace"))
        );
    }
    assert_eq!(allow_entry_problem("Shell(RM)").as_deref(), Some("rm is always denied"));
    assert_eq!(
        allow_entry_problem("Shell(/bin/rm)").as_deref(),
        Some("rm is always denied")
    );
    assert_eq!(
        allow_entry_problem("make").as_deref(),
        Some("make isn't Shell(…), Read(…) or Write(…)")
    );
}

#[test]
fn valid_entries_pass() {
    for e in ["Shell(make)", "Read(docs/**)", "Write(src/**)"] {
        assert_eq!(allow_entry_problem(e), None);
    }
}

#[test]
fn entries_split_on_commas_and_lines() {
    assert_eq!(
        split_allow_entries("Shell(make), Read(a)\n\n Write(b) ,"),
        ["Shell(make)", "Read(a)", "Write(b)"]
    );
}
