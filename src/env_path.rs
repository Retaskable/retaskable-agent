//! Reading `PATH` into an ordered list of directories, the one step every "find this binary on
//! PATH" lookup needs before it can search them. Previously duplicated verbatim across
//! `resolve_claude_on_path`, `resolve_pi_on_path`, `resolve_cursor_on_path` and `find_adb_on_path`.
use std::path::PathBuf;

/// The directories on `PATH`, in order, the same way the OS searches them.
pub fn path_dirs(env: &impl Fn(&str) -> Option<String>) -> Vec<PathBuf> {
    let path = env("PATH").unwrap_or_default();
    std::env::split_paths(&path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_dirs_splits_the_path_env_var_in_order() {
        let sep = if cfg!(windows) { ";" } else { ":" };
        let joined = format!("C:/a{sep}C:/b");
        let env = |k: &str| if k == "PATH" { Some(joined.clone()) } else { None };
        assert_eq!(path_dirs(&env), vec![PathBuf::from("C:/a"), PathBuf::from("C:/b")]);
    }

    #[test]
    fn path_dirs_is_a_single_empty_entry_when_path_is_unset() {
        let env = |_: &str| None;
        assert_eq!(path_dirs(&env), vec![PathBuf::new()]);
    }
}
