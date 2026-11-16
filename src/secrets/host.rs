//! What saved secrets touch outside this module: the process environment every child inherits, and `gh`'s git login.
//! A trait so a test gives the secrets an environment of its own and records what would have run.
use std::process::{Command, Stdio};
use std::sync::Mutex;

pub trait Host: Send + Sync {
    /// The variable `name`, or `None` when it is unset or empty (compose passes an unset `${X:-}` as empty).
    fn get(&self, name: &str) -> Option<String>;
    fn set(&self, name: &str, value: &str);
    fn remove(&self, name: &str);
    /// `gh auth setup-git`, so git pushes over https use the `GH_TOKEN` now in the environment.
    fn git_login(&self);
}

/// No environment at all, for the settings page when it edits another program's secrets: it reads and writes that
/// program's file and must change nothing of its own process.
pub struct NoHost;

impl Host for NoHost {
    fn get(&self, _: &str) -> Option<String> {
        None
    }
    fn set(&self, _: &str, _: &str) {}
    fn remove(&self, _: &str) {}
    fn git_login(&self) {}
}

/// This process's own environment, which every session, check and script the agent starts inherits.
pub struct ProcessHost;

/// Changes to the environment go one at a time. Rust makes `set_var` unsafe because a C thread reading the environment
/// as it changes is undefined behaviour; Rust's own reads and spawns take the same lock inside std, and this agent's
/// threads call no C that reads it. It is first used before anything spawns (`Secrets::apply`), and afterwards only
/// when a secret is saved or cleared on the settings page.
static ENV_WRITE: Mutex<()> = Mutex::new(());

impl Host for ProcessHost {
    fn get(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    fn set(&self, name: &str, value: &str) {
        let _guard = crate::sync::lock(&ENV_WRITE);
        // SAFETY: see `ENV_WRITE`.
        unsafe { std::env::set_var(name, value) };
    }

    fn remove(&self, name: &str) {
        let _guard = crate::sync::lock(&ENV_WRITE);
        // SAFETY: see `ENV_WRITE`.
        unsafe { std::env::remove_var(name) };
    }

    fn git_login(&self) {
        let args = ["auth", "setup-git"];
        if crate::host_call::guard_program("gh", None, &args).is_err() {
            return;
        }
        // Its output is dropped: nothing `gh` prints here is the agent's to keep.
        let status = Command::new("gh")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => tracing::info!("git now uses the saved GH_TOKEN (gh auth setup-git)."),
            _ => tracing::warn!("gh auth setup-git didn't succeed; git pushes over https may ask for a login."),
        }
    }
}
