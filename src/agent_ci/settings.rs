//! docs/specs/agent-ci.md section 2: what `retaskable-ci` runs on, read from its environment. There is no config
//! file and no project list: the repos to check come from `AGENT_CI_REPO`.
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::env::Env;

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
/// Slots when `AGENT_CI_SLOTS` is not set.
pub const DEFAULT_SLOTS: u32 = 2;
/// CPUs a check may use when `AGENT_CI_CPUS` is not set, before the cap at what Docker has.
pub const DEFAULT_CPUS: u32 = 8;
/// The branch checked when a repo names none.
pub const DEFAULT_BASE: &str = "main";

/// One GitHub repo CI runs for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// `owner/repo`.
    pub github_repo: String,
    /// The branch pull requests merge into, and whose own pushes are checked.
    pub base_branch: String,
}

impl Repo {
    /// `owner/repo`, or `owner/repo@branch` for a base other than `main`.
    pub fn parse(spec: &str) -> Result<Self> {
        let (name, base) = spec.split_once('@').unwrap_or((spec, DEFAULT_BASE));
        let ok = |part: &str| {
            !part.is_empty()
                && part.chars().any(|c| c != '.')
                && part
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        match name.split_once('/') {
            Some((owner, repo)) if ok(owner) && ok(repo) && !base.trim().is_empty() => Ok(Self {
                github_repo: name.to_string(),
                base_branch: base.trim().to_string(),
            }),
            _ => bail!("{spec:?} is not a GitHub repo: write owner/repo, or owner/repo@branch"),
        }
    }

    /// The name its folders and logs go by: `owner-repo`.
    pub fn id(&self) -> String {
        self.github_repo.replace('/', "-")
    }

    /// The URL its bare mirror fetches from.
    pub fn url(&self) -> String {
        format!("https://github.com/{}.git", self.github_repo)
    }
}

/// Everything the loop needs that is not a program.
#[derive(Debug, Clone)]
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

fn number(env: &Env, name: &str, default: u32) -> Result<u32> {
    match env.get(name) {
        None => Ok(default),
        Some(v) => v
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("{name} is {v:?}: it must be a whole number")),
    }
}

/// The comma-separated `AGENT_CI_REPO`.
pub fn parse_repos(list: &str) -> Result<Vec<Repo>> {
    let mut repos: Vec<Repo> = Vec::new();
    for spec in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let repo = Repo::parse(spec)?;
        if !repos.contains(&repo) {
            repos.push(repo);
        }
    }
    Ok(repos)
}

impl Settings {
    pub fn from_env(env: &Env) -> Result<Self> {
        let repos = parse_repos(&env.get("AGENT_CI_REPO").unwrap_or_default())?;
        if repos.is_empty() {
            bail!("AGENT_CI_REPO names no repo: set it to owner/repo, comma-separated for several");
        }
        let computer = env
            .get("AGENT_CI_NAME")
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| crate::link::machine_name(env));
        Ok(Self {
            repos,
            slots: number(env, "AGENT_CI_SLOTS", DEFAULT_SLOTS)?,
            cpus: number(env, "AGENT_CI_CPUS", DEFAULT_CPUS)?.max(1),
            computer,
            data_dir: data_dir(env),
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

    #[test]
    fn a_repo_is_owner_slash_name_with_main_unless_it_says_otherwise() {
        let r = Repo::parse("Retaskable/Retaskable.com").unwrap();
        assert_eq!(
            (r.github_repo.as_str(), r.base_branch.as_str(), r.id().as_str()),
            ("Retaskable/Retaskable.com", "main", "Retaskable-Retaskable.com")
        );
        assert_eq!(Repo::parse("o/p@trunk").unwrap().base_branch, "trunk");
        for bad in ["p", "o/", "/p", "o/p/q", "o/p@", "o p/q", "../x"] {
            assert!(Repo::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_repo_list_is_comma_separated_and_drops_blanks_and_repeats() {
        let list = parse_repos(" a/b , ,c/d,a/b").unwrap();
        assert_eq!(
            list.iter().map(|r| r.github_repo.as_str()).collect::<Vec<_>>(),
            ["a/b", "c/d"]
        );
        assert!(parse_repos("a/b,nope").is_err());
    }

    #[test]
    fn defaults_are_two_slots_eight_cpus_and_the_hostname() {
        let env = Env::from_pairs([("AGENT_CI_REPO", "o/p"), ("RETASKABLE_DATA_DIR", "d")]);
        let s = Settings::from_env(&env).unwrap();
        assert_eq!((s.slots, s.cpus), (2, 8));
        assert_eq!(s.computer, crate::link::machine_name(&env));
        assert_eq!(s.data_dir, PathBuf::from("d").join("ci"));
        assert_eq!(
            s.mirror(&s.repos[0]),
            PathBuf::from("d").join("ci").join("mirrors").join("o-p.git")
        );
    }

    #[test]
    fn the_environment_sets_slots_cpus_and_name_and_zero_slots_is_allowed() {
        let env = Env::from_pairs([
            ("AGENT_CI_REPO", "o/p"),
            ("AGENT_CI_SLOTS", "0"),
            ("AGENT_CI_CPUS", "3"),
            ("AGENT_CI_NAME", "box"),
            ("RETASKABLE_DATA_DIR", "d"),
        ]);
        let s = Settings::from_env(&env).unwrap();
        assert_eq!((s.slots, s.cpus, s.computer.as_str()), (0, 3, "box"));
    }

    #[test]
    fn no_repo_or_a_bad_number_is_an_error() {
        assert!(Settings::from_env(&Env::from_pairs([("RETASKABLE_DATA_DIR", "d")])).is_err());
        let env = Env::from_pairs([("AGENT_CI_REPO", "o/p"), ("AGENT_CI_SLOTS", "many")]);
        assert!(Settings::from_env(&env).is_err());
    }
}
