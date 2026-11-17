//! Where a project's own files live under the data directory, following `agent/src/paths.ts` in
//! Retaskable.com (`projectDir`, `runDir`), and which data folder each of the three programs (the code agent, the chat
//! agent, the CI runner) uses. This module imports none of them, so each can be installed and run on its own.
use std::path::{Path, PathBuf};

use crate::env::Env;
use crate::release::{self, Build};

/// The chat agent's own folder inside the data folder.
pub const CHAT_SUBDIR: &str = "chat";
/// The CI runner's own folder inside the data folder.
pub const CI_SUBDIR: &str = "ci";

/// The one data folder rule, as the code agent has always had it (installers/03 section 3.3): `RETASKABLE_DATA_DIR`
/// when set, else a release build's own place in the OS, else `.retaskable`. `build` is `None` in a checkout.
pub fn data_dir(env: &Env, build: Option<&Build>) -> PathBuf {
    match build {
        Some(b) if env.get("RETASKABLE_DATA_DIR").is_none() => release::data_dir(b, env),
        _ => crate::config::data_dir(env),
    }
}

/// `data_dir` for the program running now: its `build.json` is looked up beside `exe`. A `build.json` that can't be read
/// counts as a checkout here; the code agent refuses to start on it, so only a program run alone can meet it.
pub fn data_dir_for_exe(env: &Env, exe: Option<&Path>) -> PathBuf {
    let build = exe.and_then(|e| release::load_build(e).ok().flatten());
    data_dir(env, build.as_ref())
}

/// The data folder of the program running now, from its own executable.
pub fn current_data_dir(env: &Env) -> PathBuf {
    data_dir_for_exe(env, std::env::current_exe().ok().as_deref())
}

/// The code agent's own folder: the data folder itself.
pub fn code_dir(root: &Path) -> PathBuf {
    root.to_path_buf()
}

/// The chat agent's folder under the data folder.
pub fn chat_dir(root: &Path) -> PathBuf {
    root.join(CHAT_SUBDIR)
}

/// The CI runner's folder under the data folder.
pub fn ci_dir(root: &Path) -> PathBuf {
    root.join(CI_SUBDIR)
}

/// A project's own folder under the data directory: its state and transcripts.
pub fn project_dir(data_dir: &Path, project: &str) -> PathBuf {
    data_dir.join("projects").join(project)
}

/// Where one item's session transcripts live (`runDir`).
pub fn run_dir(data_dir: &Path, project: &str, item_id: &str) -> PathBuf {
    project_dir(data_dir, project).join("runs").join(item_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build() -> Build {
        serde_json::from_str(r#"{"os":"linux","version":"1.2.3","commit":"abc","environment":"production"}"#).unwrap()
    }

    #[test]
    fn a_checkout_uses_the_env_dir_else_dot_retaskable() {
        let set = Env::from_pairs([("RETASKABLE_DATA_DIR", "d")]);
        assert_eq!(data_dir(&set, None), PathBuf::from("d"));
        assert_eq!(data_dir(&Env::empty(), None), PathBuf::from(".retaskable"));
    }

    #[test]
    fn a_release_build_uses_the_os_place_unless_the_env_dir_is_set() {
        let b = build();
        let set = Env::from_pairs([("RETASKABLE_DATA_DIR", "d")]);
        assert_eq!(data_dir(&set, Some(&b)), PathBuf::from("d"));
        let bare = Env::from_pairs([("HOME", "/h"), ("LOCALAPPDATA", "/h/lad")]);
        assert_eq!(data_dir(&bare, Some(&b)), release::data_dir(&b, &bare));
        assert_ne!(data_dir(&bare, Some(&b)), PathBuf::from(".retaskable"));
    }

    #[test]
    fn a_build_json_beside_the_exe_makes_a_release_build() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("agent");
        std::fs::write(
            tmp.path().join("build.json"),
            r#"{"os":"linux","version":"1.2.3","commit":"abc","environment":"production"}"#,
        )
        .unwrap();
        let bare = Env::from_pairs([("HOME", "/h"), ("LOCALAPPDATA", "/h/lad")]);
        assert_eq!(data_dir_for_exe(&bare, Some(&exe)), release::data_dir(&build(), &bare));
        let set = Env::from_pairs([("RETASKABLE_DATA_DIR", "d")]);
        assert_eq!(data_dir_for_exe(&set, Some(&exe)), PathBuf::from("d"));
        assert_eq!(ci_dir(Path::new("d")), PathBuf::from("d").join("ci"));
        assert_eq!(chat_dir(Path::new("d")), PathBuf::from("d").join("chat"));
        assert_eq!(code_dir(Path::new("d")), PathBuf::from("d"));
    }

    #[test]
    fn no_build_json_beside_the_exe_is_a_checkout() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("agent");
        assert_eq!(
            data_dir_for_exe(&Env::empty(), Some(&exe)),
            PathBuf::from(".retaskable")
        );
        assert_eq!(data_dir_for_exe(&Env::empty(), None), PathBuf::from(".retaskable"));
    }

    #[test]
    fn project_dir_nests_under_projects() {
        let data_dir = Path::new("/data");
        assert_eq!(
            project_dir(data_dir, "my-project"),
            Path::new("/data/projects/my-project")
        );
    }

    #[test]
    fn run_dir_nests_under_the_project_and_runs() {
        let data_dir = Path::new("/data");
        assert_eq!(
            run_dir(data_dir, "my-project", "item-42"),
            Path::new("/data/projects/my-project/runs/item-42")
        );
    }

    #[test]
    fn different_projects_do_not_collide() {
        let data_dir = Path::new("/data");
        assert_ne!(
            run_dir(data_dir, "project-a", "item-1"),
            run_dir(data_dir, "project-b", "item-1")
        );
    }
}
