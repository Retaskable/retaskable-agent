//! A status file each program keeps in its own folder (docs/specs/programs-status.md): the code agent, the chat agent
//! and the CI runner each write `<its folder>/status.json` about every [`INTERVAL`], and a reader (the shared settings
//! page, later) turns the file into [`State`]. It is a file, not a channel: no program asks another anything, so each can
//! be installed and run alone. A neutral module: it imports none of the three.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::env::Env;

/// The file's name inside a program's folder.
pub const FILE: &str = "status.json";
/// How often a program writes it.
pub const INTERVAL: Duration = Duration::from_secs(10);
/// A file older than this many intervals was left by a program that is no longer running.
pub const STALE_AFTER_INTERVALS: u64 = 3;

pub const CODE_AGENT: &str = "code-agent";
pub const CHAT_AGENT: &str = "chat-agent";
pub const CI_RUNNER: &str = "ci-runner";

/// The longest `lastError` kept, so a long error text never fills the file.
const MAX_ERROR_CHARS: usize = 300;

/// Which links this program holds. `cloud` is left out by a program that has none (the CI runner).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Linked {
    pub store: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud: Option<bool>,
}

/// What the file says. Times are milliseconds since 1970. `extra` holds the program's own fields, written beside the
/// common ones.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramStatus {
    pub program: String,
    pub version: String,
    pub pid: u32,
    pub started_at: u64,
    pub updated_at: u64,
    pub linked: Linked,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// Names only, never values.
    #[serde(default)]
    pub env_set: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What one write carries besides the identity the writer already has.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub linked: Linked,
    pub last_error: Option<String>,
    pub env_set: Vec<String>,
    /// The program's own fields; a name the common fields use is ignored.
    pub extra: Map<String, Value>,
}

/// Which of `names` are set in `env`: the names, so the file never holds a value.
pub fn env_set(env: &Env, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter(|n| env.get(n).is_some())
        .map(|n| n.to_string())
        .collect()
}

pub fn now_ms() -> u64 {
    crate::telemetry::now_ms().max(0) as u64
}

/// Writes one program's file.
#[derive(Debug, Clone)]
pub struct StatusWriter {
    file: PathBuf,
    program: &'static str,
    version: String,
    pid: u32,
    started_at: u64,
}

impl StatusWriter {
    /// For the program running now, started at `started_at`; `folder` is its own folder.
    pub fn new(folder: &Path, program: &'static str, version: &str, started_at: u64) -> Self {
        Self {
            file: folder.join(FILE),
            program,
            version: version.to_string(),
            pid: std::process::id(),
            started_at,
        }
    }

    /// What `write` puts in the file at `now`.
    pub fn status(&self, now: u64, report: Report) -> ProgramStatus {
        let mut extra = report.extra;
        for common in [
            "program",
            "version",
            "pid",
            "startedAt",
            "updatedAt",
            "linked",
            "lastError",
            "envSet",
        ] {
            extra.remove(common);
        }
        ProgramStatus {
            program: self.program.to_string(),
            version: self.version.clone(),
            pid: self.pid,
            started_at: self.started_at,
            updated_at: now,
            linked: report.linked,
            last_error: report.last_error.map(|e| e.chars().take(MAX_ERROR_CHARS).collect()),
            env_set: report.env_set,
            extra,
        }
    }

    /// Writes the file whole or not at all: a temp file in the same folder, then a rename over it.
    pub fn write(&self, now: u64, report: Report) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(&self.status(now, report))?;
        replace_file(&self.file, &text)
    }

    /// `write`, with a failure only logged: a status file must never stop the program that writes it.
    pub fn write_or_log(&self, now: u64, report: Report) {
        if let Err(e) = self.write(now, report) {
            tracing::debug!("status file {}: {e}", self.file.display());
        }
    }

    /// Writes a fresh report every [`INTERVAL`] until the process ends, for a program with no loop of its own to call
    /// `write` from.
    pub fn spawn(self, report: impl Fn() -> Report + Send + 'static) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(INTERVAL);
            loop {
                tick.tick().await;
                self.write_or_log(now_ms(), report());
            }
        })
    }
}

/// A reader may hold the old file open (on Windows a rename over it then fails), so the rename is tried again briefly.
fn replace_file(file: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = file.with_file_name(format!("{FILE}.tmp"));
    std::fs::write(&tmp, text)?;
    let mut tries = 0;
    loop {
        match std::fs::rename(&tmp, file) {
            Ok(()) => return Ok(()),
            Err(e) if tries >= 5 => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
            Err(_) => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// What the settings page shows about a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    Running,
    /// Not running; the last time it wrote (ms since 1970), if a file says.
    Stopped(Option<u64>),
    /// No folder and no status file: nothing of it on this computer.
    NotInstalled,
    /// Running, but not linked to the shared backlog.
    Unlinked,
}

impl ProgramStatus {
    /// Pure: from `updatedAt` against `now` (ms). The pid is not used here: a pid can be reused, so it is only a hint to
    /// show beside a state, never what decides one.
    pub fn state(&self, now: u64) -> State {
        let stale_ms = INTERVAL.as_millis() as u64 * STALE_AFTER_INTERVALS;
        if now.saturating_sub(self.updated_at) > stale_ms {
            State::Stopped(Some(self.updated_at))
        } else if !self.linked.store {
            State::Unlinked
        } else {
            State::Running
        }
    }
}

/// The file in `folder`, or `None` when there is none or it can't be read.
pub fn read(folder: &Path) -> Option<ProgramStatus> {
    let text = std::fs::read_to_string(folder.join(FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// A program's state from its folder: no folder and no file is `NotInstalled`; a folder with no readable file is a
/// program that never ran this version, `Stopped` with nothing seen.
pub fn state_in(folder: &Path, now: u64) -> State {
    match read(folder) {
        Some(status) => status.state(now),
        None if folder.exists() => State::Stopped(None),
        None => State::NotInstalled,
    }
}

/// Every program's state, by name, under one data folder.
pub fn states(root: &Path, now: u64) -> BTreeMap<&'static str, State> {
    use crate::paths;
    BTreeMap::from([
        (CODE_AGENT, state_in(&paths::code_dir(root), now)),
        (CHAT_AGENT, state_in(&paths::chat_dir(root), now)),
        (CI_RUNNER, state_in(&paths::ci_dir(root), now)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn writer(dir: &Path) -> StatusWriter {
        StatusWriter::new(dir, CI_RUNNER, "1.2.3", 1_000)
    }

    fn report(store: bool) -> Report {
        Report {
            linked: Linked { store, cloud: None },
            ..Report::default()
        }
    }

    #[test]
    fn write_replaces_the_file_whole_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let w = writer(dir.path());
        w.write(2_000, report(true)).unwrap();
        w.write(3_000, report(true)).unwrap();
        assert_eq!(read(dir.path()).unwrap().updated_at, 3_000);
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [FILE]);
    }

    #[test]
    fn write_makes_a_missing_folder() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("a").join("ci");
        writer(&folder).write(1, report(true)).unwrap();
        assert!(read(&folder).is_some());
    }

    #[test]
    fn the_file_has_the_common_fields_and_the_programs_own() {
        let dir = tempfile::tempdir().unwrap();
        let mut extra = Map::new();
        extra.insert("docker".into(), json!("ok"));
        extra.insert("pid".into(), json!(0)); // A common name is ignored.
        let r = Report {
            linked: Linked {
                store: true,
                cloud: Some(false),
            },
            last_error: Some("x".repeat(1000)),
            env_set: vec!["AGENT_CI_REPO".into()],
            extra,
        };
        writer(dir.path()).write(2_000, r).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join(FILE)).unwrap()).unwrap();
        assert_eq!(v["program"], "ci-runner");
        assert_eq!(v["version"], "1.2.3");
        assert_eq!(v["pid"], std::process::id());
        assert_eq!(
            (v["startedAt"].clone(), v["updatedAt"].clone()),
            (json!(1000), json!(2000))
        );
        assert_eq!(v["linked"], json!({"store": true, "cloud": false}));
        assert_eq!(v["lastError"].as_str().unwrap().len(), MAX_ERROR_CHARS);
        assert_eq!(v["envSet"], json!(["AGENT_CI_REPO"]));
        assert_eq!(v["docker"], "ok");
    }

    #[test]
    fn optional_fields_are_left_out() {
        let dir = tempfile::tempdir().unwrap();
        writer(dir.path()).write(1, report(false)).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(dir.path().join(FILE)).unwrap()).unwrap();
        assert_eq!(v["linked"], json!({"store": false}));
        assert!(v.get("lastError").is_none());
        assert_eq!(v["envSet"], json!([]));
    }

    #[test]
    fn only_names_of_set_variables_are_listed_never_values() {
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let env = Env::from_pairs([("AGENT_CI_REPO", secret)]);
        let names = env_set(&env, &["AGENT_CI_REPO", "AGENT_CI_SLOTS"]);
        assert_eq!(names, ["AGENT_CI_REPO"]);
        let dir = tempfile::tempdir().unwrap();
        let r = Report {
            env_set: names,
            ..Report::default()
        };
        writer(dir.path()).write(1, r).unwrap();
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(!text.contains(secret) && !text.contains("ghp_"));
    }

    fn status(updated_at: u64, store: bool) -> ProgramStatus {
        writer(Path::new("x")).status(updated_at, report(store))
    }

    #[test]
    fn a_file_is_stale_after_three_intervals() {
        let s = status(100_000, true);
        assert_eq!(s.state(100_000), State::Running);
        assert_eq!(s.state(130_000), State::Running);
        assert_eq!(s.state(130_001), State::Stopped(Some(100_000)));
        // A clock that went back is not stale.
        assert_eq!(s.state(50_000), State::Running);
    }

    #[test]
    fn a_running_program_with_no_store_link_is_unlinked_and_a_stopped_one_is_stopped() {
        assert_eq!(status(100_000, false).state(105_000), State::Unlinked);
        assert_eq!(status(100_000, false).state(200_000), State::Stopped(Some(100_000)));
    }

    #[test]
    fn the_pid_does_not_decide_the_state() {
        let mut s = status(100_000, true);
        s.pid = u32::MAX;
        assert_eq!(s.state(105_000), State::Running);
    }

    #[test]
    fn no_folder_and_no_file_is_not_installed_and_a_bare_folder_is_stopped() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(state_in(&dir.path().join("none"), 1), State::NotInstalled);
        assert_eq!(state_in(dir.path(), 1), State::Stopped(None));
        std::fs::write(dir.path().join(FILE), "not json").unwrap();
        assert_eq!(state_in(dir.path(), 1), State::Stopped(None));
        writer(dir.path()).write(1_000, report(true)).unwrap();
        assert_eq!(state_in(dir.path(), 2_000), State::Running);
    }

    #[test]
    fn states_reads_each_programs_own_folder() {
        let root = tempfile::tempdir().unwrap();
        let now = now_ms();
        StatusWriter::new(&crate::paths::ci_dir(root.path()), CI_RUNNER, "1", now)
            .write(now, report(true))
            .unwrap();
        let all = states(root.path(), now);
        assert_eq!(all[CI_RUNNER], State::Running);
        assert_eq!(all[CHAT_AGENT], State::NotInstalled);
        assert_eq!(all[CODE_AGENT], State::Stopped(None));
    }
}
