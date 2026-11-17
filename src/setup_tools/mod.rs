//! Install and Sign in on Getting started (setup-in-app.md sections 5 and 6): the `install_tool` and `tool_sign_in`
//! commands run that tool's own fixed steps here, with no admin password and nothing from the website but the tool's
//! name. Every program runs with an argument list and no shell. What it's doing goes up in `hello.setup`; its output goes
//! to the log only. Ported from `agent/src/installTool.ts`, `ownGh.ts`, `ownGit.ts` and `toolSignIn.ts`.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::hello_settings::HelloSettings;
use crate::setup_check::{SetupCheckRunner, SetupChecks};

mod install;
mod own;
mod run;
mod sign_in;

pub use own::*;
pub use run::*;
pub use sign_in::{claude_link, device_code};

/// The only names `install_tool` takes: the website sends one of these, never a command.
pub const TOOLS: [&str; 3] = ["git", "gh", "claude"];

/// A tool as the page names it.
pub fn tool_name(tool: &str) -> &'static str {
    match tool {
        "git" => "git",
        "gh" => "the GitHub CLI",
        _ => "Claude Code",
    }
}

/// A sign-in's tool as the page names it.
pub fn sign_in_name(tool: &str) -> &'static str {
    if tool == "gh" { "GitHub" } else { "Claude" }
}

/// An install or a sign-in running now (`SetupRunning`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Running {
    tool: String,
    action: &'static str,
    since: String,
    note: Option<String>,
}

/// How the last one ended (`SetupLast`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Last {
    tool: String,
    action: &'static str,
    ok: bool,
    detail: String,
    at: String,
}

#[derive(Default)]
pub(crate) struct State {
    install: Option<Running>,
    sign_in: Option<Running>,
    last_install: Option<Last>,
    last_sign_in: Option<Last>,
}

/// What a test replaces: how programs run, downloads arrive, archives unpack, a page opens, the person is asked,
/// time passes, and the checks run again.
pub struct Deps {
    pub run: Arc<RunFn>,
    pub download: Arc<DownloadFn>,
    pub extract: Arc<ExtractFn>,
    /// A page in the default browser.
    pub open: Arc<dyn Fn(&str) + Send + Sync>,
    /// The OS's own prompt at the computer: what was typed, or `None` when cancelled.
    pub ask: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub wait: Arc<dyn Fn(Duration) + Send + Sync>,
    /// The checks run again; what they found.
    pub refresh: Arc<dyn Fn() -> Option<SetupChecks> + Send + Sync>,
    /// Puts a fetched copy first on PATH.
    pub use_gh: Arc<dyn Fn(&std::path::Path) + Send + Sync>,
    pub use_git: Arc<dyn Fn(&std::path::Path) + Send + Sync>,
    pub use_local_bin: Arc<dyn Fn(&std::path::Path) + Send + Sync>,
    pub log: Arc<dyn Fn(&str) + Send + Sync>,
}

/// Where the tools are installed and signed in: one at a time, machine-wide.
pub struct Setup {
    pub(crate) checks: Arc<SetupCheckRunner>,
    hello: HelloSettings,
    data: PathBuf,
    home: PathBuf,
    /// `linux`, `macos` or `windows`, and the processor, as `std::env::consts` spells them.
    os: String,
    arch: String,
    os_release: Option<(String, Vec<String>)>,
    state: Mutex<State>,
    deps: Deps,
}

impl std::fmt::Debug for Setup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Setup(..)")
    }
}

/// One is the same as itself and no other.
impl PartialEq for Setup {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(self, other)
    }
}

impl Setup {
    /// The real thing: the checks' runner, the hello to send again when something changes, and the data folder.
    pub fn new(checks: Arc<SetupCheckRunner>, hello: HelloSettings, data: PathBuf) -> Arc<Self> {
        let runner = checks.clone();
        let deps = Deps {
            run: Arc::new(run_program),
            download: Arc::new(download_bytes),
            extract: Arc::new(|archive, to| extract_archive(&run_program, archive, to)),
            open: Arc::new(|url| {
                let _ = crate::link::open_url(url);
            }),
            ask: Arc::new(sign_in::ask_at_computer),
            wait: Arc::new(std::thread::sleep),
            refresh: Arc::new(move || {
                let runner = runner.clone();
                tokio::runtime::Handle::try_current()
                    .ok()
                    .map(|h| h.block_on(runner.run_now()))
            }),
            use_gh: Arc::new(use_own_gh),
            use_git: Arc::new(use_own_git),
            use_local_bin: Arc::new(use_local_bin),
            log: Arc::new(|line| println!("{line}")),
        };
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)
            .unwrap_or_default();
        Self::with(
            checks,
            hello,
            data,
            home,
            std::env::consts::OS.into(),
            std::env::consts::ARCH.into(),
            deps,
        )
    }

    pub fn with(
        checks: Arc<SetupCheckRunner>,
        hello: HelloSettings,
        data: PathBuf,
        home: PathBuf,
        os: String,
        arch: String,
        deps: Deps,
    ) -> Arc<Self> {
        let os_release = (os == "linux")
            .then(|| parse_os_release(std::fs::read_to_string("/etc/os-release").ok().as_deref()))
            .flatten();
        Arc::new(Self {
            checks,
            hello,
            data,
            home,
            os,
            arch,
            os_release,
            state: Mutex::default(),
            deps,
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        crate::sync::lock(&self.state)
    }

    fn now(&self) -> String {
        crate::telemetry::events::iso_utc_millis(crate::telemetry::events::now_ms())
    }

    fn log(&self, line: &str) {
        (self.deps.log)(line);
    }

    fn changed(&self) {
        self.hello.fire();
    }

    /// Whether an install is running now.
    pub fn installing(&self) -> bool {
        self.state().install.is_some()
    }

    /// The tool being signed in to, by name, for the tray's status line.
    pub fn signing_in(&self) -> Option<&'static str> {
        self.state().sign_in.as_ref().map(|r| sign_in_name(&r.tool))
    }

    /// `hello.setup` (setup-in-app.md section 5.4): whichever is running (a sign-in's wins), the later of the two last
    /// ones, and on Linux `os` from `/etc/os-release`. Sent even when empty, so the page knows this agent installs.
    pub fn activity(&self) -> Value {
        let s = self.state();
        let running = s.sign_in.as_ref().or(s.install.as_ref());
        let mut lasts: Vec<&Last> = [s.last_install.as_ref(), s.last_sign_in.as_ref()]
            .into_iter()
            .flatten()
            .collect();
        lasts.sort_by(|a, b| a.at.cmp(&b.at));
        let mut out = json!({});
        if let Some(r) = running {
            let mut v = json!({ "tool": r.tool, "action": r.action, "since": r.since });
            if let Some(note) = &r.note {
                v["note"] = json!(note);
            }
            out["running"] = v;
        }
        if let Some(l) = lasts.last() {
            out["last"] = json!({ "tool": l.tool, "action": l.action, "ok": l.ok, "detail": l.detail, "at": l.at });
        }
        if let Some((id, like)) = &self.os_release {
            out["os"] = json!({ "id": id, "like": like });
        }
        out
    }
}

/// `ID` and `ID_LIKE` from /etc/os-release's text, or `None` when it has no `ID`.
pub fn parse_os_release(text: Option<&str>) -> Option<(String, Vec<String>)> {
    let text = text?;
    let value = |key: &str| {
        text.lines().find_map(|l| {
            l.strip_prefix(key)
                .and_then(|r| r.strip_prefix('='))
                .map(|v| v.trim().trim_matches(['"', '\'']).to_lowercase())
        })
    };
    let id = value("ID").filter(|id| !id.is_empty())?;
    let like = value("ID_LIKE")
        .unwrap_or_default()
        .split_whitespace()
        .take(10)
        .map(|s| s.chars().take(50).collect())
        .collect();
    Some((id.chars().take(50).collect(), like))
}

/// The one command that installs git on this Linux, which needs sudo, so the person copies it.
pub fn linux_git_command(os: Option<&(String, Vec<String>)>) -> &'static str {
    let ids: Vec<&str> = os
        .map(|(id, like)| std::iter::once(id).chain(like).map(String::as_str).collect())
        .unwrap_or_default();
    if ids.iter().any(|i| ["fedora", "rhel", "centos"].contains(i)) {
        "sudo dnf install git"
    } else if ids.iter().any(|i| ["arch", "manjaro"].contains(i)) {
        "sudo pacman -S git"
    } else {
        "sudo apt install git"
    }
}

#[cfg(test)]
mod tests;
