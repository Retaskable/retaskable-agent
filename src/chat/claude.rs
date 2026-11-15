//! Running `claude -p` for the Chat Agent, behind a trait so tests never start one. The Chat Agent reads only what it is
//! given (the board state in the prompt), so its sessions carry `--tools ""`: no Read, Glob, Grep, Bash or anything else
//! that touches code, on top of the engine's own `--permission-mode auto`.
use std::future::Future;
use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

use crate::engines::claude::ClaudeEngine;
use crate::engines::{
    CouldNotStart, Engine, EngineId, EngineReader, SessionEvent, SessionResult, SessionSpec, Watch, run_session,
};

/// What a session runs on. One real implementation, [`ClaudeRunner`]; tests give canned results.
pub trait Runner: Send + Sync {
    /// A Claude Code binary was found on this computer (only it has Haiku).
    fn available(&self) -> bool;
    /// Runs one session. `Err` is a session that could not start at all.
    fn run(&self, spec: SessionSpec) -> impl Future<Output = Result<SessionResult>> + Send;
}

/// `claude.rs`'s engine with `--tools ""` added: the tool list is empty, not just denied piece by piece.
struct NoTools(ClaudeEngine);

impl Engine for NoTools {
    fn id(&self) -> EngineId {
        EngineId::Claude
    }
    fn binary(&self, env: &crate::env::Env, windows: bool, dirs: &[PathBuf]) -> Result<PathBuf> {
        self.0.binary(env, windows, dirs)
    }
    fn args(&self, spec: &SessionSpec) -> Vec<String> {
        no_tools(self.0.args(spec))
    }
    fn env(&self, spec: &SessionSpec) -> Vec<(String, String)> {
        self.0.env(spec)
    }
    fn prompt(&self, spec: &SessionSpec) -> String {
        self.0.prompt(spec)
    }
    fn reader(&self, spec: &SessionSpec) -> Box<dyn EngineReader> {
        self.0.reader(spec)
    }
    fn summarize(&self, event: &Value, at: i64) -> Vec<SessionEvent> {
        self.0.summarize(event, at)
    }
}

/// `args` with the empty tool list at the end (after the variadic `--disallowedTools`, which it would otherwise join).
pub fn no_tools(mut args: Vec<String>) -> Vec<String> {
    args.push("--tools".to_string());
    args.push(String::new());
    args
}

/// The real runner: the Claude binary found at start.
pub struct ClaudeRunner {
    bin: Option<PathBuf>,
}

impl ClaudeRunner {
    pub fn new(env: &crate::env::Env) -> Self {
        ClaudeRunner {
            bin: crate::engines::resolve::resolve_claude_on_path(env).ok(),
        }
    }
}

impl Runner for ClaudeRunner {
    fn available(&self) -> bool {
        self.bin.is_some()
    }

    async fn run(&self, spec: SessionSpec) -> Result<SessionResult> {
        let Some(bin) = &self.bin else {
            return Err(anyhow::Error::new(CouldNotStart(
                "no Claude Code on this computer".into(),
            )));
        };
        run_session(&NoTools(ClaudeEngine), bin, &spec, &new_id(), None, Watch::default()).await
    }
}

fn new_id() -> String {
    format!("chat-{:016x}", (crate::connector::random() * u64::MAX as f64) as u64)
}

/// A scratch directory with nothing in it for a session to start in (no repository: the Chat Agent has none).
pub fn scratch_dir() -> PathBuf {
    let dir = std::env::temp_dir().join("retaskable-chat-agent");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

#[cfg(test)]
pub(crate) mod fake {
    use super::*;
    use std::sync::Mutex;

    /// A runner that answers from a queue and keeps the specs it was given.
    pub struct Fake {
        pub available: bool,
        pub results: Mutex<Vec<Result<SessionResult>>>,
        pub specs: Mutex<Vec<SessionSpec>>,
    }

    impl Fake {
        pub fn new(results: Vec<Result<SessionResult>>) -> Self {
            Fake {
                available: true,
                results: Mutex::new(results.into_iter().rev().collect()),
                specs: Mutex::new(Vec::new()),
            }
        }
    }

    impl Runner for Fake {
        fn available(&self) -> bool {
            self.available
        }
        async fn run(&self, spec: SessionSpec) -> Result<SessionResult> {
            self.specs.lock().unwrap().push(spec);
            self.results.lock().unwrap().pop().expect("an unexpected session")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_tool_list_is_emptied_last() {
        let args = no_tools(vec!["-p".into(), "--disallowedTools".into(), "Bash".into()]);
        assert_eq!(&args[args.len() - 2..], ["--tools", ""]);
        assert_eq!(args[0], "-p");
    }

    #[test]
    fn the_real_args_keep_auto_permissions_and_no_bypass() {
        let spec = crate::engines::engine::test_spec(Path::new("."), "x");
        let args = no_tools(NoTools(ClaudeEngine).args(&spec));
        let i = args.iter().position(|a| a == "--permission-mode").unwrap();
        assert_eq!(args[i + 1], "auto");
        assert!(
            !args
                .iter()
                .any(|a| a.contains("dangerously") || a == "bypassPermissions")
        );
        assert_eq!(args.last().unwrap(), "");
    }
}
