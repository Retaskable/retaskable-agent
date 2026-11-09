//! Host calls (`gh`, `glab`) with a missing argument are refused and recorded (agent-telemetry.md section 3.3,
//! `host_call_undefined`). A call deep in the code has no event log, so a refusal goes to a process-wide sink that
//! the orchestrator's pass drains into the log of the project it was for ([`crate::orchestrator::driver`]).
//! Behind the `host-call-undefined-event` flag: Off ([`set_on_for`]), the guard neither refuses nor records.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::telemetry::events::{MAX_TEXT, scrub};

/// The release flag.
pub const FLAG: &str = "host-call-undefined-event";

// Tests run in parallel threads and each pass sets this, so under test it is per thread.
#[cfg(not(test))]
static ON: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
#[cfg(test)]
thread_local!(static ON: Mutex<Vec<PathBuf>> = const { Mutex::new(Vec::new()) });

/// Sets the repos whose project has the flag On; the orchestrator does it each pass. A call is guarded only when
/// its directory is under one of them, so a project with the flag Off keeps today's behaviour.
pub fn set_on_for(repos: Vec<PathBuf>) {
    #[cfg(test)]
    ON.with(|o| *o.lock().unwrap_or_else(|p| p.into_inner()) = repos);
    #[cfg(not(test))]
    {
        *ON.lock().unwrap_or_else(|p| p.into_inner()) = repos;
    }
}

fn is_on(dir: Option<&Path>) -> bool {
    // A call with no directory (the repo picker, the profile) is guarded when any project has the flag On.
    let under = |o: &Mutex<Vec<PathBuf>>| {
        let on = o.lock().unwrap_or_else(|p| p.into_inner());
        dir.map_or(!on.is_empty(), |d| on.iter().any(|r| d.starts_with(r)))
    };
    #[cfg(test)]
    return ON.with(under);
    #[cfg(not(test))]
    under(&ON)
}

/// One refused call: the command, its arguments (scrubbed and cut) and the directory it would have run in.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub cmd: String,
    pub args: Vec<String>,
    pub dir: Option<PathBuf>,
}

/// The call was not run: this argument (counting from 1) was undefined.
#[derive(Debug, Clone, PartialEq)]
pub struct Refused {
    pub cmd: String,
    pub n: usize,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} was not run: argument {} was undefined", self.cmd, self.n)
    }
}

impl std::error::Error for Refused {}

/// A shared list of [`Refusal`]s; clones share it.
#[derive(Debug, Clone, Default)]
pub struct Sink(Arc<Mutex<Vec<Refusal>>>);

impl Sink {
    /// The process-wide sink (per thread under test, like the flag).
    #[cfg(not(test))]
    pub fn global() -> Sink {
        static SINK: std::sync::OnceLock<Sink> = std::sync::OnceLock::new();
        SINK.get_or_init(Sink::default).clone()
    }

    #[cfg(test)]
    pub fn global() -> Sink {
        thread_local!(static SINK: Sink = Sink::default());
        SINK.with(Sink::clone)
    }

    pub fn add(&self, refusal: Refusal) {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).push(refusal);
    }

    /// Everything added since the last call.
    pub fn take(&self) -> Vec<Refusal> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(|p| p.into_inner()))
    }
}

/// The index of the first argument that is empty, or is `undefined` or `NaN` whole or as one part of a path or
/// value in it (parts split on `/`, `=`, `:`, `,`, `#` and whitespace).
pub fn undefined_arg(args: &[&str]) -> Option<usize> {
    args.iter().position(|a| {
        a.is_empty()
            || a.split(|c: char| matches!(c, '/' | '=' | ':' | ',' | '#') || c.is_whitespace())
                .any(|p| p == "undefined" || p == "NaN")
    })
}

/// Checks a call before it runs: on a match (and with the flag On) records it in the global sink and refuses.
pub fn guard(cmd: &str, dir: &Path, args: &[&str]) -> Result<(), Refused> {
    guard_in(cmd, Some(dir), args)
}

/// [`guard`] for a call that may have no directory: then any project with the flag On covers it.
pub fn guard_in(cmd: &str, dir: Option<&Path>, args: &[&str]) -> Result<(), Refused> {
    if !is_on(dir) {
        return Ok(());
    }
    let Some(i) = undefined_arg(args) else {
        return Ok(());
    };
    Sink::global().add(Refusal {
        cmd: cmd.into(),
        args: args.iter().map(|a| scrub(a, MAX_TEXT)).collect(),
        dir: dir.map(Path::to_path_buf),
    });
    Err(Refused {
        cmd: cmd.into(),
        n: i + 1,
    })
}

/// A program's name for the event: its file stem (`gh`, `glab`), never a full path.
pub fn stem(program: &std::ffi::OsStr) -> String {
    let path = Path::new(program);
    path.file_stem().unwrap_or(program).to_string_lossy().into_owned()
}

/// For the call sites' tests: a fake `gh`/`glab` that only leaves a marker, the guard On for its directory.
#[cfg(all(test, unix))]
pub(crate) mod testing {
    use super::*;

    /// A program `name` in `dir` that touches `dir/ran`; turns the guard On for `dir`.
    pub fn fake(dir: &Path, name: &str) -> PathBuf {
        let program = dir.join(name);
        crate::test_support::write_executable(&program, &format!("#!/bin/sh\ntouch '{}'\n", dir.join("ran").display()));
        let _ = std::fs::remove_file(dir.join("ran")); // `write_executable` ran it once
        set_on_for(vec![dir.to_path_buf()]);
        program
    }

    /// Whether the fake ran, and the refusals recorded since the last call.
    pub fn after(dir: &Path) -> (bool, Vec<Refusal>) {
        (dir.join("ran").exists(), Sink::global().take())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_an_empty_or_undefined_argument() {
        assert_eq!(undefined_arg(&["pr", "checks", "undefined"]), Some(2));
        assert_eq!(undefined_arg(&["NaN"]), Some(0));
        assert_eq!(undefined_arg(&["pr", ""]), Some(1));
        assert_eq!(undefined_arg(&["api", "repos/o/r/pulls/undefined"]), Some(1));
        assert_eq!(undefined_arg(&["--field=pr=NaN"]), Some(0));
        assert_eq!(undefined_arg(&["pr=undefined"]), Some(0));
        assert_eq!(undefined_arg(&["a,NaN"]), Some(0));
        assert_eq!(undefined_arg(&["x#undefined"]), Some(0));
        assert_eq!(undefined_arg(&["a b:undefined"]), Some(0));
    }

    #[test]
    fn near_misses_pass() {
        assert_eq!(
            undefined_arg(&["undefinedness", "NaNa", "nan", "feature/banana", "pr=7", "--json"]),
            None
        );
        assert_eq!(undefined_arg(&[]), None);
    }
}
