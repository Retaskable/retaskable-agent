//! Reading `PATH` the way every `resolve_*_on_path` / `find_*_on_path` lookup does
//! (`engines::resolve_claude_on_path`, `resolve_pi_on_path`, `setup_check::resolve_cursor_on_path`,
//! `phone::find_adb_on_path`): split into directories, with a missing `PATH` reading as empty
//! rather than erroring.
use std::path::PathBuf;

pub fn path_dirs(env: &impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let path = env("PATH").unwrap_or_default();
    std::env::split_paths(&path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_dirs_splits_on_the_platform_separator() {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let joined = format!("/a{sep}/b");
        let dirs = path_dirs(&|key: &str| (key == "PATH").then(|| joined.clone()));
        assert_eq!(dirs, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn path_dirs_reads_as_a_single_empty_entry_when_path_is_missing() {
        // `std::env::split_paths("")` yields one empty component, not zero; this just has to
        // match that, since every existing `resolve_*_on_path` already relied on it.
        let dirs = path_dirs(&|_: &str| None);
        assert_eq!(dirs, vec![PathBuf::from("")]);
    }
}
