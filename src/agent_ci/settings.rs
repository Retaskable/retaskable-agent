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
pub const ENV_VARS: [&str; 8] = [
    "AGENT_CI_REPO",
    FROM_AGENT_VAR,
    "AGENT_CI_SLOTS",
    "AGENT_CI_CPUS",
    "AGENT_CI_NAME",
    "RETASKABLE_DATA_DIR",
    "RETASKABLE_BACKLOG_URL",
    "RETASKABLE_CI_CREDENTIALS_FILE",
];
/// `1`: the repos to check are the projects on this computer's code agent, read from the shared data folder
/// (docs/specs/setup-wizard.md section 2.3), and a computer with none yet is not a misread setup: the runner idles.
pub const FROM_AGENT_VAR: &str = "RETASKABLE_CI_FROM_AGENT";
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
        let mut file = ci::read(&data_dir)?;
        let from_agent = env.get(FROM_AGENT_VAR).is_some_and(|v| v.trim() == "1");
        // Neither the environment nor the runner's own file names a repo: the agent's projects are the list.
        let named = env.get(ci::REPOS.env).is_some_and(|v| !v.trim().is_empty());
        if from_agent && !named && file.repos.is_empty() {
            file.repos = data_dir.parent().map(agents_repos).unwrap_or_default();
        }
        let r = if from_agent && !named && file.repos.is_empty() {
            // No project yet: resolve with a stand-in repo for the numbers, then check nothing.
            let mut stand_in = file.clone();
            stand_in.repos = vec![ci::Repo::parse("none/none")?];
            let mut r = ci::resolve(env, &stand_in)?;
            r.repos.clear();
            r
        } else {
            ci::resolve(env, &file)?
        };
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

/// The repos of the projects in the code agent's `config.json` under `root` (its `githubRepo`, on its `baseBranch` when
/// it names one), skipping any that isn't `owner/repo`. Read as plain JSON: the runner names nothing of the agent's code.
pub fn agents_repos(root: &Path) -> Vec<Repo> {
    let text = std::fs::read_to_string(root.join("config.json")).unwrap_or_default();
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut repos: Vec<Repo> = Vec::new();
    for p in json["projects"].as_array().into_iter().flatten() {
        let name = p["githubRepo"].as_str().unwrap_or_default();
        let base = p["baseBranch"].as_str().filter(|b| !b.trim().is_empty());
        let spec = match base {
            Some(base) => format!("{name}@{}", base.trim()),
            None => name.to_string(),
        };
        if let Ok(repo) = Repo::parse(&spec)
            && !repos.contains(&repo)
        {
            repos.push(repo);
        }
    }
    repos
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
    fn from_the_agent_the_repos_are_its_projects_and_none_yet_is_an_idle_runner() {
        let root = tempfile::tempdir().unwrap();
        let e = env(root.path(), &[(FROM_AGENT_VAR, "1")]);
        let idle = Settings::load(&e).unwrap();
        assert!(idle.repos.is_empty(), "nothing to check yet is not an error");
        std::fs::write(
            root.path().join("config.json"),
            r#"{"projects":[{"githubRepo":"o/p"},{"githubRepo":"o/q","baseBranch":"dev"},{"githubRepo":"bad"},{"githubRepo":"o/p"}]}"#,
        )
        .unwrap();
        let s = Settings::load(&e).unwrap();
        let specs: Vec<String> = s.repos.iter().map(Repo::spec).collect();
        assert_eq!(specs, ["o/p", "o/q@dev"]);
        // The environment's list, then the runner's own file, win over the agent's projects.
        let named = env(root.path(), &[(FROM_AGENT_VAR, "1"), ("AGENT_CI_REPO", "x/y")]);
        assert_eq!(Settings::load(&named).unwrap().repos[0].spec(), "x/y");
        // Without the variable a runner with no repo still refuses to start.
        assert!(Settings::load(&env(root.path(), &[])).is_err());
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
