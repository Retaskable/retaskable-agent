//! Host calls (`gh`, `glab`) with a missing argument are refused and recorded (agent-telemetry.md section 3.3,
//! `host_call_undefined`). A call deep in the code has no event log, so a refusal goes to a process-wide sink that
//! the orchestrator's pass drains into the log of the project it was for ([`crate::orchestrator::driver`]).
//! Behind the `host-call-undefined-event` flag: Off ([`set_on_for`]), the guard neither refuses nor records.
use std::path::{Path, PathBuf};
#[cfg(not(test))]
use std::sync::OnceLock;
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

fn is_on(dir: &Path) -> bool {
    let under = |o: &Mutex<Vec<PathBuf>>| {
        o.lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|r| dir.starts_with(r))
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
    /// The process-wide sink.
    pub fn global() -> &'static Sink {
        // Tests run in parallel threads, so under test it is per thread (like `ON`).
        #[cfg(test)]
        {
            thread_local!(static SINK: &'static Sink = Box::leak(Box::default()));
            SINK.with(|s| *s)
        }
        #[cfg(not(test))]
        {
            static SINK: OnceLock<Sink> = OnceLock::new();
            SINK.get_or_init(Sink::default)
        }
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

/// Checks a call to `program` (recorded as its file stem, `gh` or `glab`, never a path) before it runs. With no
/// `dir` the call is guarded when any project has the flag On, and its refusal is written to every project's log.
pub fn guard_program(program: impl AsRef<Path>, dir: Option<&Path>, args: &[&str]) -> Result<(), Refused> {
    let cmd = program
        .as_ref()
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    match dir {
        Some(dir) => guard(&cmd, dir, args),
        None => {
            if !any_on() {
                return Ok(());
            }
            let Some(i) = undefined_arg(args) else {
                return Ok(());
            };
            Sink::global().add(Refusal {
                cmd: cmd.clone(),
                args: args.iter().map(|a| scrub(a, MAX_TEXT)).collect(),
                dir: None,
            });
            Err(Refused { cmd, n: i + 1 })
        }
    }
}

fn any_on() -> bool {
    let some = |o: &Mutex<Vec<PathBuf>>| !o.lock().unwrap_or_else(|p| p.into_inner()).is_empty();
    #[cfg(test)]
    return ON.with(some);
    #[cfg(not(test))]
    some(&ON)
}

/// Checks a call before it runs: on a match (and with the flag On) records it in the global sink and refuses.
pub fn guard(cmd: &str, dir: &Path, args: &[&str]) -> Result<(), Refused> {
    if !is_on(dir) {
        return Ok(());
    }
    let Some(i) = undefined_arg(args) else {
        return Ok(());
    };
    Sink::global().add(Refusal {
        cmd: cmd.into(),
        args: args.iter().map(|a| scrub(a, MAX_TEXT)).collect(),
        dir: Some(dir.to_path_buf()),
    });
    Err(Refused {
        cmd: cmd.into(),
        n: i + 1,
    })
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
