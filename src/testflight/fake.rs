//! A fake of what a build asks of the computer, and a project to build: no Xcode, no npm, no Apple signing, no network.
//! It records every step, and makes and removes the throwaway checkout as `git worktree add` and `remove` would.
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::{Ran, Step, Tools, build_script, xcodeproj};
use crate::config::{DeployConfig, DeployTarget, ProjectConfig, ReleaseConfig};

/// 2023-11-14T22:13:20.000Z, build number `2311142213`.
pub const NOW: u64 = 1_700_000_000_000;
pub const BUILD_NUMBER: &str = "2311142213";
pub const SHA: &str = "abc1234def5678abc1234def5678abc1234def56";
pub const PBXPROJ: &str = "buildSettings = {\n\tCURRENT_PROJECT_VERSION = 7;\n\tMARKETING_VERSION = 1.2;\n};\n\
                           buildSettings = {\n\tCURRENT_PROJECT_VERSION = 1.0.3;\n};\n";

#[derive(Default)]
pub struct State {
    pub steps: Vec<Step>,
    pub fetches: usize,
    pub served_calls: usize,
    /// The answers to the reads of the served commit, then `served_after`.
    pub served: VecDeque<Option<String>>,
    pub served_after: Option<String>,
    pub fetch_error: Option<String>,
    /// How a program ends instead of exiting 0 (`git` is for `worktree add` only).
    pub ends: HashMap<String, Ran>,
    /// Leaves the checkout behind when asked to remove it.
    pub keep_checkout: bool,
    pub bare_checkout: bool,
    /// The pbxproj as the script saw it, and whether a build was running then.
    pub pbxproj_at_script: Option<String>,
    pub running_at_script: bool,
}

pub struct Fake {
    pub mac: bool,
    pub permission: Result<String, String>,
    pub login: Result<String, String>,
    pub state: Mutex<State>,
}

impl Fake {
    /// A Mac whose production serves [`SHA`], with Xcode, and whose `ann` is an admin.
    pub fn new() -> Self {
        Self {
            mac: true,
            permission: Ok("admin".into()),
            login: Ok("ann".into()),
            state: Mutex::new(State {
                served_after: Some(SHA.into()),
                ..State::default()
            }),
        }
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut State) -> T) -> T {
        f(&mut self.state.lock().unwrap())
    }

    /// Every step run, as `program args`.
    pub fn lines(&self) -> Vec<String> {
        self.with(|s| s.steps.iter().map(Step::line).collect())
    }

    pub fn step(&self, program: &str) -> Option<Step> {
        self.with(|s| s.steps.iter().find(|x| x.program == program).cloned())
    }
}

impl Tools for Fake {
    fn mac(&self) -> bool {
        self.mac
    }

    fn now_ms(&self) -> u64 {
        NOW
    }

    async fn login(&self) -> Result<String, String> {
        self.login.clone()
    }

    async fn permission_of(&self, _login: &str) -> Result<String, String> {
        self.permission.clone()
    }

    async fn served(&self, _url: &str) -> Option<String> {
        self.with(|s| {
            s.served_calls += 1;
            s.served.pop_front().unwrap_or_else(|| s.served_after.clone())
        })
    }

    async fn fetch(&self) -> Result<(), String> {
        self.with(|s| {
            s.fetches += 1;
            s.fetch_error.clone().map_or(Ok(()), Err)
        })
    }

    async fn run(&self, step: &Step, _log: Option<&Path>) -> Ran {
        let mut s = self.state.lock().unwrap();
        s.steps.push(step.clone());
        let ok = Ran {
            code: Some(0),
            ..Ran::default()
        };
        let args: Vec<&str> = step.args.iter().map(String::as_str).collect();
        match (step.program.as_str(), args.as_slice()) {
            ("git", ["worktree", "add", "--detach", dir, _]) => {
                if let Some(end) = s.ends.get("git") {
                    return end.clone();
                }
                let app = Path::new(dir).join("app");
                std::fs::create_dir_all(app.join(build_script()).parent().unwrap()).unwrap();
                if !s.bare_checkout {
                    std::fs::create_dir_all(app.join(xcodeproj()).parent().unwrap()).unwrap();
                    std::fs::write(app.join(xcodeproj()), PBXPROJ).unwrap();
                }
                ok
            }
            ("git", ["worktree", "remove", "--force", dir]) => {
                if !s.keep_checkout {
                    let _ = std::fs::remove_dir_all(dir);
                }
                ok
            }
            ("bash", _) => {
                s.pbxproj_at_script = std::fs::read_to_string(step.cwd.join(xcodeproj())).ok();
                s.running_at_script = super::testflight_running();
                s.ends.get("bash").cloned().unwrap_or(ok)
            }
            ("xcodebuild", _) => s.ends.get("xcodebuild").cloned().unwrap_or(ok),
            (program, _) => s.ends.get(program).cloned().unwrap_or(ok),
        }
    }
}

/// A Ran that exited `code` after printing `output`.
pub fn exited(code: i32, output: &str) -> Ran {
    Ran {
        code: Some(code),
        output: output.into(),
        ..Ran::default()
    }
}

/// A project with the committed Xcode project and the script, the setting on, and production and staging addresses.
pub struct Project {
    pub root: tempfile::TempDir,
    pub config: ProjectConfig,
}

impl Project {
    pub fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        let app = repo.join("app");
        for file in [xcodeproj(), build_script()] {
            std::fs::create_dir_all(app.join(&file).parent().unwrap()).unwrap();
            std::fs::write(app.join(&file), PBXPROJ).unwrap();
        }
        let target = |url: &str| DeployTarget {
            branch: "x".into(),
            url: Some(url.into()),
            smoke: None,
            version_path: None,
            rollout: None,
        };
        let config = ProjectConfig {
            repo_path: repo.to_string_lossy().into_owned(),
            github_repo: "acme/app".into(),
            worktrees_dir: root.path().join("wt").to_string_lossy().into_owned(),
            release: ReleaseConfig {
                testflight: true,
                testflight_any_member: false,
            },
            deploy: DeployConfig {
                merges_to: None,
                staging: Some(target("https://staging.example/")),
                production: Some(target("https://prod.example")),
            },
            ..ProjectConfig::default()
        };
        Self { root, config }
    }

    /// The project's folder for its logs.
    pub fn dir(&self) -> PathBuf {
        self.root.path().join("data")
    }

    /// Where the checkout of the build [`BUILD_NUMBER`] goes.
    pub fn checkout(&self) -> PathBuf {
        PathBuf::from(&self.config.worktrees_dir).join(format!("testflight-{BUILD_NUMBER}"))
    }
}
