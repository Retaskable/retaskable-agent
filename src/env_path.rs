//! Reading `PATH` the way every `resolve_*_on_path` / `find_*_on_path` lookup does
//! (the `engines::resolve` functions and `phone::find_adb_on_path`): split into directories, with a missing or empty `PATH` reading as no directories
//! rather than erroring.
use crate::env::Env;
use std::path::PathBuf;

pub fn path_dirs(env: &Env) -> Vec<PathBuf> {
    let path = env.get("PATH").unwrap_or_default();
    if path.is_empty() {
        // `split_paths("")` yields one empty entry, which `join` turns into a relative lookup in the
        // current directory (a planted `claude.exe` or `git.exe` there). No PATH means no directories.
        return Vec::new();
    }
    // An empty entry inside (`/a::/b`, or a trailing `:`) is the same current-directory lookup.
    std::env::split_paths(&path)
        .filter(|d| !d.as_os_str().is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_dirs_splits_on_the_platform_separator() {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let joined = format!("/a{sep}/b");
        let dirs = path_dirs(&Env::from_pairs([("PATH", joined)]));
        assert_eq!(dirs, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }

    #[test]
    fn path_dirs_is_empty_when_path_is_missing_or_empty() {
        // Not one empty entry: that would make every lookup search the current directory.
        assert!(path_dirs(&Env::empty()).is_empty());
        assert!(path_dirs(&Env::from_pairs([("PATH", "")])).is_empty());
    }

    #[test]
    fn path_dirs_drops_empty_entries_between_separators() {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let joined = format!("{sep}/a{sep}{sep}/b{sep}");
        let dirs = path_dirs(&Env::from_pairs([("PATH", joined)]));
        assert_eq!(dirs, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }
}
