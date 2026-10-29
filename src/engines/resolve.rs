//! Finding the binary for each engine: `claude`, `cursor-agent`, `pi` and `codex`. One leaf module
//! (it reads only [`crate::env_path`]), so `setup_check` and the engines both depend on it and it
//! depends on neither. Each `resolve_*` takes the environment, the platform and the `PATH`
//! directories as arguments so it can be tested; each `resolve_*_on_path` reads them itself.
use std::path::PathBuf;

use anyhow::Result;

/// `resolveClaude()`: finds the real `claude` binary to run, following docs/specs/agent/
/// 04-architecture.md's "Finding Claude" — on Windows the `claude` on PATH is an npm shim batch
/// file, so this looks behind it for the `claude.exe` it wraps, since spawning a `.cmd` directly
/// from a piped child process is unreliable. `RETASKABLE_CLAUDE` always wins when set.
pub fn resolve_claude(env: impl Fn(&str) -> Option<String>, windows: bool, path_dirs: &[PathBuf]) -> Result<PathBuf> {
    if let Some(over) = env("RETASKABLE_CLAUDE") {
        return Ok(PathBuf::from(over));
    }
    if !windows {
        return Ok(PathBuf::from("claude"));
    }
    for dir in path_dirs {
        let exe = dir.join("claude.exe");
        if exe.is_file() {
            return Ok(exe);
        }
        let shim = dir.join("claude.cmd");
        if shim.is_file() {
            let behind = dir
                .join("node_modules")
                .join("@anthropic-ai")
                .join("claude-code")
                .join("bin")
                .join("claude.exe");
            if behind.is_file() {
                return Ok(behind);
            }
        }
    }
    anyhow::bail!("claude.exe wasn't found on PATH behind the npm shim. Set RETASKABLE_CLAUDE to its full path.")
}

/// `resolve_claude`, reading PATH itself the way the real agent does.
pub fn resolve_claude_on_path(env: impl Fn(&str) -> Option<String>) -> Result<PathBuf> {
    let dirs = crate::env_path::path_dirs(&env);
    resolve_claude(env, cfg!(windows), &dirs)
}

/// `resolveCodex()`: `RETASKABLE_CODEX` always wins; otherwise the first `codex[.exe]` on `PATH`,
/// falling back to the bare name so a missing Codex is reported by the spawn itself.
pub fn resolve_codex(env: impl Fn(&str) -> Option<String>, windows: bool, path_dirs: &[PathBuf]) -> PathBuf {
    if let Some(over) = env("RETASKABLE_CODEX") {
        return PathBuf::from(over);
    }
    let name = if windows { "codex.exe" } else { "codex" };
    for dir in path_dirs {
        let exe = dir.join(name);
        if exe.is_file() {
            return exe;
        }
    }
    PathBuf::from(name)
}

/// `resolve_codex`, reading `PATH` itself the way the real agent does.
pub fn resolve_codex_on_path(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let dirs = crate::env_path::path_dirs(&env);
    resolve_codex(&env, cfg!(windows), &dirs)
}

/// `resolvePi()`: `RETASKABLE_PI` always wins; otherwise the first `pi[.exe]` on `PATH`. Never
/// errors (unlike `resolve_claude`): a missing pi isn't fatal, so this falls back to the bare name
/// and lets the spawn itself report "not installed".
pub fn resolve_pi(env: impl Fn(&str) -> Option<String>, windows: bool, path_dirs: &[PathBuf]) -> PathBuf {
    if let Some(over) = env("RETASKABLE_PI") {
        return PathBuf::from(over);
    }
    let name = if windows { "pi.exe" } else { "pi" };
    for dir in path_dirs {
        let exe = dir.join(name);
        if exe.is_file() {
            return exe;
        }
    }
    PathBuf::from(name)
}

/// `resolve_pi`, reading `PATH` itself the way the real agent does.
pub fn resolve_pi_on_path(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let dirs = crate::env_path::path_dirs(&env);
    resolve_pi(&env, cfg!(windows), &dirs)
}

/// `resolveCursor()` (`agent/src/engines/cursor.ts`): `RETASKABLE_CURSOR` always wins; otherwise
/// the first `cursor-agent[.exe]` (or, on Windows, `cursor-agent.cmd`) on PATH, then
/// `~/.local/bin`. Never errors (unlike `resolve_claude`): a missing Cursor isn't fatal, so this
/// falls back to the bare name and lets the spawn itself report "not installed".
pub fn resolve_cursor(
    env: impl Fn(&str) -> Option<String>,
    windows: bool,
    path_dirs: &[PathBuf],
    home: Option<PathBuf>,
) -> PathBuf {
    if let Some(over) = env("RETASKABLE_CURSOR") {
        return PathBuf::from(over);
    }
    let name = if windows { "cursor-agent.exe" } else { "cursor-agent" };
    for dir in path_dirs {
        let exe = dir.join(name);
        if exe.is_file() {
            return exe;
        }
        if windows {
            let shim = dir.join("cursor-agent.cmd");
            if shim.is_file() {
                return shim;
            }
        }
    }
    if let Some(home) = home {
        let fallback = home.join(".local").join("bin").join(name);
        if fallback.is_file() {
            return fallback;
        }
    }
    PathBuf::from(name)
}

/// `resolve_cursor`, reading PATH and the home directory itself the way the real agent does.
pub fn resolve_cursor_on_path(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let dirs = crate::env_path::path_dirs(&env);
    let home = env("HOME").or_else(|| env("USERPROFILE")).map(PathBuf::from);
    resolve_cursor(&env, cfg!(windows), &dirs, home)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_claude_prefers_the_env_override_on_any_platform() {
        let env = |k: &str| (k == "RETASKABLE_CLAUDE").then(|| "C:/custom/claude.exe".to_string());
        assert_eq!(
            resolve_claude(env, true, &[]).unwrap(),
            PathBuf::from("C:/custom/claude.exe")
        );
        assert_eq!(
            resolve_claude(env, false, &[]).unwrap(),
            PathBuf::from("C:/custom/claude.exe")
        );
    }

    #[test]
    fn resolve_claude_is_plain_claude_off_windows() {
        let no_env = |_: &str| None;
        assert_eq!(resolve_claude(no_env, false, &[]).unwrap(), PathBuf::from("claude"));
    }

    #[test]
    fn resolve_claude_finds_claude_exe_directly_on_the_path_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("claude.exe"), "").unwrap();
        let no_env = |_: &str| None;
        let got = resolve_claude(no_env, true, &[dir.path().to_path_buf()]).unwrap();
        assert_eq!(got, dir.path().join("claude.exe"));
    }

    #[test]
    fn resolve_claude_goes_behind_the_npm_shim_on_windows() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("claude.cmd"), "").unwrap();
        let behind = dir
            .path()
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code")
            .join("bin");
        std::fs::create_dir_all(&behind).unwrap();
        std::fs::write(behind.join("claude.exe"), "").unwrap();
        let no_env = |_: &str| None;
        let got = resolve_claude(no_env, true, &[dir.path().to_path_buf()]).unwrap();
        assert_eq!(got, behind.join("claude.exe"));
    }

    #[test]
    fn resolve_claude_fails_naming_the_fix_when_nothing_matches_on_windows() {
        let no_env = |_: &str| None;
        let err = resolve_claude(no_env, true, &[]).unwrap_err();
        assert!(err.to_string().contains("RETASKABLE_CLAUDE"));
    }

    #[test]
    fn resolve_claude_on_path_reads_dirs_from_the_path_env_on_windows() {
        if !cfg!(windows) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("claude.exe"), "").unwrap();
        let path_value = dir.path().to_string_lossy().into_owned();
        let env = move |k: &str| (k == "PATH").then(|| path_value.clone());
        assert_eq!(resolve_claude_on_path(env).unwrap(), dir.path().join("claude.exe"));
    }

    #[test]
    fn resolve_codex_prefers_the_env_then_the_path_then_the_bare_name() {
        let env = |k: &str| (k == "RETASKABLE_CODEX").then(|| "C:/custom/codex.exe".to_string());
        assert_eq!(resolve_codex(env, false, &[]), PathBuf::from("C:/custom/codex.exe"));
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("codex"), "").unwrap();
        let none = |_: &str| None;
        assert_eq!(
            resolve_codex(none, false, &[dir.path().to_path_buf()]),
            dir.path().join("codex")
        );
        assert_eq!(resolve_codex(none, false, &[]), PathBuf::from("codex"));
        assert_eq!(resolve_codex(none, true, &[]), PathBuf::from("codex.exe"));
    }

    #[test]
    fn resolve_pi_prefers_the_env_override_on_any_platform() {
        let env = |k: &str| (k == "RETASKABLE_PI").then(|| "C:/custom/pi.exe".to_string());
        assert_eq!(resolve_pi(env, true, &[]), PathBuf::from("C:/custom/pi.exe"));
        assert_eq!(resolve_pi(env, false, &[]), PathBuf::from("C:/custom/pi.exe"));
    }

    #[test]
    fn resolve_pi_finds_it_on_the_path_or_falls_back_to_the_bare_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pi"), "").unwrap();
        let no_env = |_: &str| None;
        assert_eq!(
            resolve_pi(no_env, false, &[dir.path().to_path_buf()]),
            dir.path().join("pi")
        );
        assert_eq!(resolve_pi(no_env, false, &[]), PathBuf::from("pi"));
        assert_eq!(resolve_pi(no_env, true, &[]), PathBuf::from("pi.exe"));
    }

    #[test]
    fn resolve_pi_on_path_reads_dirs_from_the_path_env() {
        let dir = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) { "pi.exe" } else { "pi" };
        std::fs::write(dir.path().join(name), "").unwrap();
        let path_value = dir.path().to_string_lossy().into_owned();
        let env = move |k: &str| (k == "PATH").then(|| path_value.clone());
        assert_eq!(resolve_pi_on_path(env), dir.path().join(name));
    }

    #[test]
    fn resolve_cursor_prefers_the_env_override_on_any_platform() {
        let env = |k: &str| (k == "RETASKABLE_CURSOR").then(|| "C:/custom/cursor-agent.exe".to_string());
        assert_eq!(
            resolve_cursor(env, true, &[], None),
            PathBuf::from("C:/custom/cursor-agent.exe")
        );
        assert_eq!(
            resolve_cursor(env, false, &[], None),
            PathBuf::from("C:/custom/cursor-agent.exe")
        );
    }

    #[test]
    fn resolve_cursor_finds_cursor_agent_directly_on_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) {
            "cursor-agent.exe"
        } else {
            "cursor-agent"
        };
        std::fs::write(dir.path().join(name), "").unwrap();
        let no_env = |_: &str| None;
        let got = resolve_cursor(no_env, cfg!(windows), &[dir.path().to_path_buf()], None);
        assert_eq!(got, dir.path().join(name));
    }

    #[test]
    fn resolve_cursor_falls_back_to_the_cmd_shim_on_windows() {
        if !cfg!(windows) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("cursor-agent.cmd"), "").unwrap();
        let no_env = |_: &str| None;
        let got = resolve_cursor(no_env, true, &[dir.path().to_path_buf()], None);
        assert_eq!(got, dir.path().join("cursor-agent.cmd"));
    }

    #[test]
    fn resolve_cursor_falls_back_to_home_local_bin() {
        let home = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) {
            "cursor-agent.exe"
        } else {
            "cursor-agent"
        };
        let bin_dir = home.path().join(".local").join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::write(bin_dir.join(name), "").unwrap();
        let no_env = |_: &str| None;
        let got = resolve_cursor(no_env, cfg!(windows), &[], Some(home.path().to_path_buf()));
        assert_eq!(got, bin_dir.join(name));
    }

    #[test]
    fn resolve_cursor_falls_back_to_the_bare_name_when_nothing_matches() {
        let no_env = |_: &str| None;
        let name = if cfg!(windows) {
            "cursor-agent.exe"
        } else {
            "cursor-agent"
        };
        assert_eq!(resolve_cursor(no_env, cfg!(windows), &[], None), PathBuf::from(name));
    }

    #[test]
    fn resolve_cursor_on_path_reads_dirs_from_the_path_env() {
        let dir = tempfile::tempdir().unwrap();
        let name = if cfg!(windows) {
            "cursor-agent.exe"
        } else {
            "cursor-agent"
        };
        std::fs::write(dir.path().join(name), "").unwrap();
        let path_value = dir.path().to_string_lossy().into_owned();
        let env = move |k: &str| (k == "PATH").then(|| path_value.clone());
        assert_eq!(resolve_cursor_on_path(env), dir.path().join(name));
    }
}
