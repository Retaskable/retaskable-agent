//! Every process the agent starts goes through here (installers/03 section 3.2: "nothing it starts may open a window"):
//! on Windows a child gets `CREATE_NO_WINDOW`, so none flashes a console, least of all under the desktop app. Elsewhere
//! the flag doesn't exist and these are plain `Command`s. The one place a person asked for a window (Play, `play.rs`)
//! uses a plain `Command` and says so; [`tests::every_process_starts_through_the_hidden_helper`] fails any other.
use std::ffi::OsStr;
use std::process::Command;

/// Windows `CREATE_NO_WINDOW`.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A command for `program` that opens no window on Windows.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// [`command`] for tokio; `kill_on_drop` is the caller's to set.
pub fn tokio_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    tokio::process::Command::from(command(program))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_command_runs_the_program_it_was_given() {
        assert_eq!(command("git").get_program(), "git");
        assert_eq!(tokio_command("gh").as_std().get_program(), "gh");
    }

    /// Source files that may call `Command::new` themselves, and why.
    const OWN: &[(&str, &str)] = &[
        ("spawn.rs", "the helper"),
        ("low_priority.rs", "adds BELOW_NORMAL to the same no-window flag"),
        ("release_install.rs", "sets CREATE_NO_WINDOW | DETACHED_PROCESS itself"),
        ("tray/helper/client.rs", "sets CREATE_NO_WINDOW itself"),
        ("play.rs", "Play opens a window on purpose: a person asked for it"),
        ("bin/bundle.rs", "a build tool run from a terminal"),
        ("package.rs", "a build tool run from a terminal"),
        ("release_check.rs", "a build tool run from a terminal"),
        ("test_support.rs", "test helpers"),
        // Held back while their own changes merge; each is one line to move onto the helper.
        ("engines/runner.rs", "TODO: claude, cursor-agent, pi, codex"),
        ("orchestrator/piece_runs.rs", "TODO"),
        ("smoke.rs", "TODO"),
    ];

    fn rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    #[test]
    fn every_process_starts_through_the_hidden_helper() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rs_files(&src, &mut files);
        let mut missing = Vec::new();
        for f in files {
            let rel = f.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
            let name = rel.rsplit('/').next().unwrap();
            let is_test = name.ends_with("_test.rs")
                || name.ends_with("_tests.rs")
                || name == "tests.rs"
                || rel.split('/').any(|d| d == "tests");
            if is_test || OWN.iter().any(|(o, _)| *o == rel) {
                continue;
            }
            let text = std::fs::read_to_string(&f).unwrap();
            // Production code only: a file's own test module comes last.
            let prod = text.split("\n#[cfg(test)]\nmod ").next().unwrap();
            for (n, line) in prod.lines().enumerate() {
                let t = line.trim_start();
                if !t.starts_with("//") && t.contains("Command::new(") {
                    missing.push(format!("{rel}:{}: {}", n + 1, t));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "start these through crate::spawn::command / tokio_command so Windows opens no window:\n{}",
            missing.join("\n")
        );
    }
}
