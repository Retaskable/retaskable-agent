//! Running a program for an install or a sign-in: an argument list, no shell, a time limit, its output kept.
//! `agent/src/proc.ts`'s `run`, without the git hooks.
use std::io::Read;
use std::process::Stdio;
use std::time::{Duration, Instant};

/// How a program ended.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ran {
    /// `None` when it couldn't start or was stopped.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

/// Runs a program: `(program, args, time limit)`. Replaced in tests.
pub type RunFn = dyn Fn(&str, &[String], Duration) -> Ran + Send + Sync;

/// Runs `program` with `args` (no shell), stopped after `limit`.
pub fn run_program(program: &str, args: &[String], limit: Duration) -> Ran {
    let mut cmd = crate::spawn::command(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ran {
                stderr: format!("couldn't start {program}: {e}"),
                ..Ran::default()
            };
        }
    };
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut p) = pipe {
                let mut bytes = Vec::new();
                let _ = p.read_to_end(&mut bytes);
                s = String::from_utf8_lossy(&bytes).into_owned();
            }
            s
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() >= limit => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => break None,
        }
    };
    Ran {
        code: status.and_then(|s| s.code()),
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
        timed_out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn keeps_the_output_and_the_exit_code() {
        let r = run_program(
            "sh",
            &["-c".into(), "echo hi; echo oops >&2; exit 3".into()],
            Duration::from_secs(10),
        );
        assert_eq!(
            (r.code, r.stdout.trim(), r.stderr.trim(), r.timed_out),
            (Some(3), "hi", "oops", false)
        );
    }

    #[cfg(unix)]
    #[test]
    fn stops_a_program_that_runs_too_long() {
        let r = run_program("sleep", &["30".into()], Duration::from_millis(200));
        assert!(r.timed_out);
        assert_ne!(r.code, Some(0));
    }

    #[test]
    fn a_program_that_isnt_there_says_so() {
        let r = run_program("retaskable-no-such-program", &[], Duration::from_secs(1));
        assert_eq!(r.code, None);
        assert!(r.stderr.starts_with("couldn't start"));
    }
}
