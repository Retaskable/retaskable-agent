//! docs/specs/agent-ci.md section 3.7: what `retaskable-ci` runs on, read from its environment and its own
//! `<data folder>/ci/settings.json` (the schema and the layering are `program_settings::ci`, shared with the settings
//! page). There is no project list: the repos to check come from `AGENT_CI_REPO` or the file.
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::env::Env;
use crate::program_settings::ci;

/// The CI runner's own folder inside the data folder: its link marker, mirrors, check directories and logs.
pub const DATA_SUBDIR: &str = crate::paths::CI_SUBDIR;
/// The environment variables that set the runner (the status file lists which are set, never their values).
pub const ENV_VARS: [&str; 6] = [
    "AGENT_CI_REPO",
    "AGENT_CI_SLOTS",
    "AGENT_CI_CPUS",
    "AGENT_CI_NAME",
    "RETASKABLE_DATA_DIR",
    "RETASKABLE_BACKLOG_URL",
];
pub use crate::program_settings::ci::{DEFAULT_BASE, DEFAULT_CPUS, DEFAULT_SLOTS, Repo, parse_repos};

/// Everything the loop needs that is not a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub repos: Vec<Repo>,
    /// Checks at once; 0 runs nothing.
    pub slots: u32,
    /// `--cpus` for each check, before the cap at Docker's CPU count.
    pub cpus: u32,
    /// The name in each status description: `agent-ci <computer>: ...`.
    pub computer: String,
    /// The runner's own data folder (`<data dir>/ci`).
    pub data_dir: PathBuf,
}

/// The data folder the agent on this computer uses, apart from the CI runner's subfolder: found the way the code agent
/// finds it (`paths::data_dir`).
pub fn data_root(env: &Env) -> PathBuf {
    crate::paths::current_data_dir(env)
}

/// The runner's own folder.
pub fn data_dir(env: &Env) -> PathBuf {
    crate::paths::ci_dir(&data_root(env))
}

/// `data_dir` for a given executable, so a test can say where its `build.json` is.
pub fn data_dir_for(env: &Env, exe: Option<&Path>) -> PathBuf {
    crate::paths::ci_dir(&crate::paths::data_dir_for_exe(env, exe))
}

impl Settings {
    /// The settings now: the environment over `<ci folder>/settings.json`. The runner calls this again each poll, so
    /// a change on the page applies without a restart. Only `data_dir` is bound at start (it is where the file is).
    pub fn load(env: &Env) -> Result<Self> {
        Self::load_in(env, data_dir(env))
    }

    /// `load` for a runner folder that is already known.
    pub fn load_in(env: &Env, data_dir: PathBuf) -> Result<Self> {
        let file = ci::read(&data_dir)?;
        let r = ci::resolve(env, &file)?;
        Ok(Self {
            repos: r.repos,
            slots: r.slots,
            cpus: r.cpus,
            computer: r.name.unwrap_or_else(|| crate::link::machine_name(env)),
            data_dir,
        })
    }

    /// Where a repo's bare mirror lives.
    pub fn mirror(&self, repo: &Repo) -> PathBuf {
        mirror_dir(&self.data_dir, repo)
    }
}

pub fn mirror_dir(data_dir: &Path, repo: &Repo) -> PathBuf {
    data_dir.join("mirrors").join(format!("{}.git", repo.id()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(dir: &Path, pairs: &[(&str, &str)]) -> Env {
        let mut all: Vec<(String, String)> = vec![("RETASKABLE_DATA_DIR".into(), dir.to_str().unwrap().into())];
        all.extend(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        Env::from_pairs(all)
    }

    #[test]
    fn defaults_are_two_slots_eight_cpus_and_the_hostname() {
        let e = Env::from_pairs([("AGENT_CI_REPO", "o/p"), ("RETASKABLE_DATA_DIR", "d")]);
        let s = Settings::load(&e).unwrap();
        assert_eq!((s.slots, s.cpus), (2, 8));
        assert_eq!(s.computer, crate::link::machine_name(&e));
        assert_eq!(s.data_dir, PathBuf::from("d").join("ci"));
        assert_eq!(
            s.mirror(&s.repos[0]),
            PathBuf::from("d").join("ci").join("mirrors").join("o-p.git")
        );
    }

    #[test]
    fn the_environment_sets_slots_cpus_and_name_and_zero_slots_is_allowed() {
        let e = Env::from_pairs([
            ("AGENT_CI_REPO", "o/p"),
            ("AGENT_CI_SLOTS", "0"),
            ("AGENT_CI_CPUS", "3"),
            ("AGENT_CI_NAME", "box"),
            ("RETASKABLE_DATA_DIR", "d"),
        ]);
        let s = Settings::load(&e).unwrap();
        assert_eq!((s.slots, s.cpus, s.computer.as_str()), (0, 3, "box"));
    }

    #[test]
    fn no_repo_or_a_bad_number_is_an_error() {
        assert!(Settings::load(&Env::from_pairs([("RETASKABLE_DATA_DIR", "d")])).is_err());
        let e = Env::from_pairs([("AGENT_CI_REPO", "o/p"), ("AGENT_CI_SLOTS", "many")]);
        assert!(Settings::load(&e).is_err());
    }

    #[test]
    fn the_file_in_the_ci_folder_is_read_again_on_each_load_and_the_environment_wins() {
        let root = tempfile::tempdir().unwrap();
        let ci_dir = root.path().join("ci");
        std::fs::create_dir_all(&ci_dir).unwrap();
        let file = ci_dir.join("settings.json");
        std::fs::write(&file, r#"{"repos":["a/b"],"slots":4,"cpus":2,"name":"filebox"}"#).unwrap();
        let s = Settings::load(&env(root.path(), &[])).unwrap();
        assert_eq!((s.slots, s.cpus, s.computer.as_str()), (4, 2, "filebox"));
        std::fs::write(&file, r#"{"repos":["a/b","c/d@dev"],"slots":1}"#).unwrap();
        let s = Settings::load(&env(root.path(), &[("AGENT_CI_SLOTS", "6")])).unwrap();
        assert_eq!((s.slots, s.repos.len()), (6, 2));
        std::fs::write(&file, "{oops").unwrap();
        assert!(Settings::load(&env(root.path(), &[])).is_err());
    }
}
