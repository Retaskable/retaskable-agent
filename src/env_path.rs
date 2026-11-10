//! Reading `PATH` the way every `resolve_*_on_path` / `find_*_on_path` lookup does
//! (the `engines::resolve` functions and `phone::find_adb_on_path`): split into directories, with a missing `PATH` reading as empty
//! rather than erroring.
use crate::env::Env;
use std::path::PathBuf;

pub fn path_dirs(env: &Env) -> Vec<PathBuf> {
    let path = env.get("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .filter(|dir| !dir.as_os_str().is_empty())
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
    fn path_dirs_drops_empty_entries_so_nothing_is_looked_up_in_the_working_directory() {
        assert!(path_dirs(&Env::empty()).is_empty());
        let sep = if cfg!(windows) { ";" } else { ":" };
        let joined = format!("{sep}/a{sep}{sep}/b{sep}");
        let dirs = path_dirs(&Env::from_pairs([("PATH", joined)]));
        assert_eq!(dirs, vec![PathBuf::from("/a"), PathBuf::from("/b")]);
    }
}
