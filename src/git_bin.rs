//! Starting git the way docs/specs/agent/04-architecture.md section 4.1.1 ("How the agent runs
//! git") asks: on Windows, `cmd\git.exe` on PATH (Git for Windows, or Retaskable's own MinGit) is
//! a launcher that puts its `mingw64\bin` and `usr\bin` first on PATH, sets `MSYSTEM`, then starts
//! the real `mingw64\bin\git.exe` as a second process. Starting a process is most of what a git
//! call costs on Windows, so the agent's own git calls start that real git.exe itself instead,
//! with the same PATH and `MSYSTEM` for that one call (so `sh`, `ssh` and the credential helpers
//! are still found as before; git itself sets `HOME` when it's missing, as the launcher would).
//! Any other git first on PATH, and every other platform, runs plain `git`. `RETASKABLE_PLAIN_GIT`
//! turns this off. Ported from `agent/src/gitBin.ts`.
use crate::env::Env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;

/// How to start git for one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitLauncher {
    Plain,
    Real {
        exe: PathBuf,
        prepend_path: Vec<PathBuf>,
        msystem: String,
    },
}

/// Finds the launcher to use, given `windows` and the already-split `PATH` directories so this is
/// testable without touching the real environment. `RETASKABLE_PLAIN_GIT` (any value) always
/// answers [`GitLauncher::Plain`].
pub fn resolve_git(env: &Env, windows: bool, path_dirs: &[PathBuf]) -> GitLauncher {
    if env.get("RETASKABLE_PLAIN_GIT").is_some() || !windows {
        return GitLauncher::Plain;
    }
    let Some(dir) = path_dirs.iter().find(|dir| dir.join("git.exe").is_file()) else {
        return GitLauncher::Plain;
    };
    // Git for Windows and MinGit both put the launcher at `cmd\git.exe`, with the real git behind
    // it at `<install>\mingw64\bin\git.exe`. Any other git.exe first on PATH runs plain `git`.
    if dir.file_name().and_then(|n| n.to_str()) == Some("cmd")
        && let Some(install) = dir.parent()
    {
        let real = install.join("mingw64").join("bin").join("git.exe");
        if real.is_file() {
            return GitLauncher::Real {
                exe: real,
                prepend_path: vec![install.join("mingw64").join("bin"), install.join("usr").join("bin")],
                msystem: "MINGW64".to_string(),
            };
        }
    }
    GitLauncher::Plain
}

/// `resolve_git`, reading PATH itself the way the real agent does.
pub fn resolve_git_on_path(env: &Env) -> GitLauncher {
    let dirs = crate::env_path::path_dirs(env);
    resolve_git(env, cfg!(windows), &dirs)
}

/// The launcher for this process's PATH, looked up once ("It's looked up once per PATH").
static LAUNCHER: LazyLock<GitLauncher> = LazyLock::new(|| resolve_git_on_path(&Env::from_process_raw()));

/// A `Command` for running git in `dir`: the real git.exe directly when [`LAUNCHER`] found Git for
/// Windows' launcher first on PATH, otherwise plain `git`. This is the one place the agent's own
/// code should build a git `Command` from.
pub fn command(dir: &Path) -> Command {
    let mut cmd = match &*LAUNCHER {
        GitLauncher::Plain => Command::new("git"),
        GitLauncher::Real {
            exe,
            prepend_path,
            msystem,
        } => {
            let mut cmd = Command::new(exe);
            let rest = std::env::var("PATH").unwrap_or_default();
            let dirs = prepend_path.iter().cloned().chain(std::env::split_paths(&rest));
            if let Ok(new_path) = std::env::join_paths(dirs) {
                cmd.env("PATH", new_path);
            }
            cmd.env("MSYSTEM", msystem);
            cmd
        }
    };
    cmd.current_dir(dir);
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_exe(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn resolve_git_is_plain_off_windows_even_with_a_launcher_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = dir.path().join("cmd");
        write_exe(&cmd.join("git.exe"));
        write_exe(&dir.path().join("mingw64").join("bin").join("git.exe"));

        assert_eq!(resolve_git(&Env::empty(), false, &[cmd]), GitLauncher::Plain);
    }

    #[test]
    fn resolve_git_is_plain_when_retaskable_plain_git_is_set() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = dir.path().join("cmd");
        write_exe(&cmd.join("git.exe"));
        write_exe(&dir.path().join("mingw64").join("bin").join("git.exe"));
        let env = Env::from_pairs([("RETASKABLE_PLAIN_GIT", "1")]);

        assert_eq!(resolve_git(&env, true, &[cmd]), GitLauncher::Plain);
    }

    #[test]
    fn resolve_git_finds_the_real_git_behind_the_cmd_launcher_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = dir.path().join("cmd");
        write_exe(&cmd.join("git.exe"));
        let real = dir.path().join("mingw64").join("bin").join("git.exe");
        write_exe(&real);

        let launcher = resolve_git(&Env::empty(), true, &[cmd]);

        assert_eq!(
            launcher,
            GitLauncher::Real {
                exe: real,
                prepend_path: vec![
                    dir.path().join("mingw64").join("bin"),
                    dir.path().join("usr").join("bin"),
                ],
                msystem: "MINGW64".to_string(),
            }
        );
    }

    #[test]
    fn resolve_git_is_plain_when_the_cmd_launcher_has_no_real_git_behind_it() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = dir.path().join("cmd");
        write_exe(&cmd.join("git.exe"));
        // No mingw64\bin\git.exe behind it.

        assert_eq!(resolve_git(&Env::empty(), true, &[cmd]), GitLauncher::Plain);
    }

    #[test]
    fn resolve_git_is_plain_for_a_git_exe_thats_not_behind_a_cmd_launcher() {
        let dir = tempfile::tempdir().unwrap();
        write_exe(&dir.path().join("git.exe"));

        assert_eq!(
            resolve_git(&Env::empty(), true, &[dir.path().to_path_buf()]),
            GitLauncher::Plain
        );
    }

    #[test]
    fn resolve_git_on_path_reads_dirs_from_the_path_env_on_windows() {
        if !cfg!(windows) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let cmd = dir.path().join("cmd");
        write_exe(&cmd.join("git.exe"));
        let real = dir.path().join("mingw64").join("bin").join("git.exe");
        write_exe(&real);
        let path_value = cmd.to_string_lossy().into_owned();
        let env = Env::from_pairs([("PATH", path_value.clone())]);

        assert_eq!(
            resolve_git_on_path(&env),
            GitLauncher::Real {
                exe: real,
                prepend_path: vec![
                    dir.path().join("mingw64").join("bin"),
                    dir.path().join("usr").join("bin"),
                ],
                msystem: "MINGW64".to_string(),
            }
        );
    }
}
