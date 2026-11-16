//! What a build asks of the computer (`TestflightDeps` in `agent/src/testflight.ts`): a trait so the tests use a fake,
//! since the real commands need a Mac with Xcode, npm and Apple signing. [`RealTools`] is the real one.
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use crate::process_tree::{KILL_LIMIT, ProcessTree};

/// How much of a step's output is read back (for the error's last lines): its whole output is in the build log.
const TAIL_BYTES: u64 = 20_000;
/// How long `git fetch` may take.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// One fixed command: never a model's, never a shell string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// Set on top of this process's own environment.
    pub env: Vec<(String, String)>,
    pub timeout: Option<Duration>,
}

impl Step {
    pub fn new(program: &str, args: &[&str], cwd: &Path) -> Self {
        Self {
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: cwd.to_path_buf(),
            env: Vec::new(),
            timeout: None,
        }
    }

    /// The command as the build log shows it: `npm ci`.
    pub fn line(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// How a step ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    /// `None` when it never started or was killed (no exit code).
    pub code: Option<i32>,
    /// The end of what it printed, or why it couldn't start.
    pub output: String,
    pub timed_out: bool,
    /// A restart or quit cut it (its process tree was killed from outside).
    pub cut: bool,
}

impl Ran {
    pub fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

pub trait Tools: Send + Sync {
    /// Whether this is a Mac (`process.platform === "darwin"`).
    fn mac(&self) -> bool;
    /// The time now, in milliseconds since the epoch.
    fn now_ms(&self) -> u64;
    /// The login this computer's `gh` is signed in as.
    fn login(&self) -> impl Future<Output = Result<String, String>> + Send;
    /// Someone's permission on the repo: `admin`, `write`, `read` or `none`.
    fn permission_of(&self, login: &str) -> impl Future<Output = Result<String, String>> + Send;
    /// The commit a version address serves, or `None`.
    fn served(&self, url: &str) -> impl Future<Output = Option<String>> + Send;
    /// `git fetch` of the project's checkout.
    fn fetch(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// Runs one step to its end, its whole output appended to `log` when there is one. Never fails: a step that can't
    /// start is a [`Ran`] with no code and the reason in `output`.
    fn run(&self, step: &Step, log: Option<&Path>) -> impl Future<Output = Ran> + Send;
}

/// The real tools for one project.
pub struct RealTools {
    pub repo: PathBuf,
    pub github_repo: String,
    /// The `gh` program (a test passes a fake).
    pub gh: PathBuf,
}

impl Tools for RealTools {
    fn mac(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn now_ms(&self) -> u64 {
        crate::program_status::now_ms()
    }

    async fn login(&self) -> Result<String, String> {
        crate::promote::host::login(&self.gh, &self.repo).await
    }

    async fn permission_of(&self, login: &str) -> Result<String, String> {
        crate::promote::host::permission_of(&self.gh, &self.repo, &self.github_repo, login).await
    }

    async fn served(&self, url: &str) -> Option<String> {
        crate::promote::site::served_sha(url).await
    }

    async fn fetch(&self) -> Result<(), String> {
        let mut step = Step::new("git", &["fetch", "--prune", "origin"], &self.repo);
        step.timeout = Some(FETCH_TIMEOUT);
        let ran = self.run(&step, None).await;
        if ran.ok() {
            Ok(())
        } else if ran.timed_out {
            Err("git fetch timed out.".into())
        } else {
            Err(crate::proc::tail_lines(&ran.output, 5))
        }
    }

    async fn run(&self, step: &Step, log: Option<&Path>) -> Ran {
        let failed = |why: String| Ran {
            output: why,
            ..Ran::default()
        };
        // Both pipes go to the build log (appended whole, as it runs) so a 60-minute Xcode archive is never held in memory.
        let (out, err, start) = match log {
            Some(file) => {
                let open = || std::fs::OpenOptions::new().create(true).append(true).open(file);
                let Ok(out) = open() else {
                    return failed(format!("couldn't open {}", file.display()));
                };
                let start = out.metadata().map_or(0, |m| m.len());
                let Ok(err) = out.try_clone() else {
                    return failed(format!("couldn't open {}", file.display()));
                };
                (Stdio::from(out), Stdio::from(err), start)
            }
            None => (Stdio::null(), Stdio::null(), 0),
        };
        let mut cmd = tokio::process::Command::new(&step.program);
        cmd.args(&step.args)
            .current_dir(&step.cwd)
            .envs(step.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .kill_on_drop(true);
        ProcessTree::prepare(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return failed(format!("starting {}: {e}", step.program)),
        };
        let tree = match ProcessTree::join(&child) {
            Ok(t) => t,
            Err(e) => {
                let _ = child.kill().await;
                return failed(format!("putting {} in its own process tree: {e:#}", step.program));
            }
        };
        let waited = match step.timeout {
            Some(t) => tokio::time::timeout(t, child.wait()).await.ok(),
            None => Some(child.wait().await),
        };
        let timed_out = waited.is_none();
        if timed_out {
            // The whole tree, not just `npm` or `bash`: an archive spawns xcodebuild and more.
            let t = tree.clone();
            let _ = tokio::task::spawn_blocking(move || t.kill_and_wait(KILL_LIMIT)).await;
            let _ = child.wait().await;
        }
        tree.finished();
        let code = waited.and_then(Result::ok).and_then(|s| s.code());
        Ran {
            code,
            output: log.map(|f| read_tail(f, start)).unwrap_or_default(),
            timed_out,
            cut: tree.was_cut(),
        }
    }
}

/// What `file` gained after `start`, its last [`TAIL_BYTES`] bytes.
fn read_tail(file: &Path, start: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(file) else {
        return String::new();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let from = start.max(len.saturating_sub(TAIL_BYTES));
    let mut bytes = Vec::new();
    if f.seek(SeekFrom::Start(from)).is_ok() {
        let _ = f.read_to_end(&mut bytes);
    }
    String::from_utf8_lossy(&bytes).into_owned()
}
