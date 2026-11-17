//! Install (setup-in-app.md section 5): each tool's own fixed steps.
use std::sync::Arc;
use std::time::Duration;

use super::{Last, Running, Setup, own, tool_name};
use crate::setup_check::SetupChecks;

/// Each install's time limit, and git on macOS's, whose dialog the person finishes.
const INSTALL: Duration = Duration::from_secs(10 * 60);
pub const MAC_GIT_MS: u64 = 30 * 60_000;
pub const MAC_GIT_EVERY: Duration = Duration::from_secs(30);

/// Anthropic's installers, fetched only from these addresses over HTTPS.
pub const CLAUDE_INSTALLER_SH: &str = "https://claude.ai/install.sh";
pub const CLAUDE_INSTALLER_PS1: &str = "https://claude.ai/install.ps1";

pub const MAC_GIT_NOTE: &str = "Finish in the macOS window that opened: press Install.";

/// Whether the checks say the tool is there already.
fn installed(tool: &str, checks: Option<&SetupChecks>) -> bool {
    let Some(c) = checks else { return false };
    match tool {
        "git" => c.git.ok == Some(true),
        "claude" => c.claude.ok == Some(true),
        _ => c.gh_installed.ok == Some(true),
    }
}

impl Setup {
    /// Why it can't be asked now, or `None`: on arrival and again as it starts (section 5.1).
    pub fn install_refusal(&self, tool: &str) -> Option<String> {
        if installed(tool, self.checks.latest().as_ref()) {
            return Some("Already installed.".into());
        }
        if self.state().install.is_some() {
            return Some("Another install is running: wait for it.".into());
        }
        if tool == "git" && self.os == "linux" {
            return Some("Installing git needs sudo: copy the command instead.".into());
        }
        if tool == "git" && self.os != "macos" && self.os != "windows" {
            return Some("Retaskable can't install git on this computer.".into());
        }
        // One install or sign-in at a time (section 6.1).
        if self.state().sign_in.is_some() {
            return Some("A sign-in is running: wait for it.".into());
        }
        None
    }

    /// Runs the tool's steps, then the checks again. `Err` is the one line the page gets when it fails. macOS's git
    /// returns once its dialog shows, and waits for git in the background (`running` says so meanwhile). Blocks.
    pub fn install(self: &Arc<Self>, tool: &str) -> Result<(), String> {
        if let Some(refusal) = self.install_refusal(tool) {
            return Err(refusal);
        }
        self.state().install = Some(Running {
            tool: tool.into(),
            action: "install",
            since: self.now(),
            note: None,
        });
        self.changed();
        self.log(&format!("Install: installing {}", tool_name(tool)));
        let step = match tool {
            "git" if self.os == "macos" => return self.mac_git(),
            "claude" => self.claude(),
            "gh" => self.gh(),
            _ => self.windows_git(),
        };
        match step {
            Ok(()) => {
                self.end(tool, true, &format!("{} is installed.", tool_name(tool)));
                Ok(())
            }
            Err(detail) => {
                self.end(tool, false, &detail);
                Err(detail)
            }
        }
    }

    /// A request that ended before it ran: the page says so after a reload.
    pub fn install_refused(&self, tool: &str, reason: &str) {
        self.state().last_install = Some(Last {
            tool: tool.into(),
            action: "install",
            ok: false,
            detail: reason.chars().take(300).collect(),
            at: self.now(),
        });
        self.changed();
    }

    fn end(&self, tool: &str, ok: bool, detail: &str) {
        self.log(&format!("Install: {}: {detail}", tool_name(tool)));
        {
            let mut s = self.state();
            s.install = None;
            s.last_install = Some(Last {
                tool: tool.into(),
                action: "install",
                ok,
                detail: detail.chars().take(300).collect(),
                at: self.now(),
            });
        }
        let _ = (self.deps.refresh)();
        self.changed();
    }

    /// Anthropic's installer: downloaded into `<data>/downloads`, its SHA-256 logged, run, and deleted. Then
    /// `~/.local/bin`, where it puts `claude`, goes on the agent's own PATH.
    fn claude(&self) -> Result<(), String> {
        let win = self.os == "windows";
        let url = if win { CLAUDE_INSTALLER_PS1 } else { CLAUDE_INSTALLER_SH };
        let bytes = (self.deps.download)(url).map_err(|e| format!("Couldn't download Anthropic's installer: {e}"))?;
        {
            use sha2::{Digest, Sha256};
            let sha: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
            self.log(&format!("Install: {url} has SHA-256 {sha}"));
        }
        let dir = self.data.join("downloads");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(if win { "claude-install.ps1" } else { "claude-install.sh" });
        std::fs::write(&file, &bytes).map_err(|e| e.to_string())?;
        let path = file.to_string_lossy().into_owned();
        let (cmd, args): (&str, Vec<String>) = if win {
            (
                "powershell",
                ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"]
                    .iter()
                    .map(|s| s.to_string())
                    .chain([path])
                    .collect(),
            )
        } else {
            ("/bin/bash", vec![path])
        };
        let r = (self.deps.run)(cmd, &args, INSTALL);
        let _ = std::fs::remove_file(&file);
        self.log(format!("Install: {}\n{}", r.stdout, r.stderr).trim());
        if r.timed_out {
            return Err("Anthropic's installer took over 10 minutes, so it was stopped: see the log.".into());
        }
        if r.code != Some(0) {
            return Err("Anthropic's installer stopped: see the log.".into());
        }
        (self.deps.use_local_bin)(&self.home);
        Ok(())
    }

    fn gh(&self) -> Result<(), String> {
        own::fetch_own_gh(
            &self.data,
            &self.os,
            &self.arch,
            None,
            &*self.deps.download,
            &*self.deps.extract,
            |d| (self.deps.use_gh)(d),
        )
    }

    /// Retaskable's own MinGit, for every project.
    fn windows_git(&self) -> Result<(), String> {
        let dir = own::fetch_own_git(&self.data, &self.arch, None, &*self.deps.download, &*self.deps.extract)?;
        (self.deps.use_git)(&dir);
        Ok(())
    }

    /// `xcode-select --install` shows macOS's own dialog and returns; the checks run every 30 s until git answers,
    /// for 30 minutes at most. macOS saying they're installed already checks at once.
    fn mac_git(self: &Arc<Self>) -> Result<(), String> {
        let r = (self.deps.run)("xcode-select", &["--install".to_string()], Duration::from_secs(60));
        self.log(format!("Install: {}\n{}", r.stdout, r.stderr).trim());
        if r.code != Some(0) && !r.stderr.to_lowercase().contains("already installed") {
            let detail = "macOS couldn't start installing its command-line tools: see the log.".to_string();
            self.end("git", false, &detail);
            return Err(detail);
        }
        if let Some(run) = self.state().install.as_mut() {
            run.note = Some(MAC_GIT_NOTE.into());
        }
        self.changed();
        let me = self.clone();
        std::thread::spawn(move || me.wait_for_git());
        Ok(())
    }

    fn wait_for_git(&self) {
        let mut waited = 0;
        loop {
            if (self.deps.refresh)().is_some_and(|c| c.git.ok == Some(true)) {
                return self.end("git", true, "git is installed.");
            }
            if waited >= MAC_GIT_MS {
                return self.end(
                    "git",
                    false,
                    "git still wasn't installed after 30 minutes. Press Install to try again.",
                );
            }
            (self.deps.wait)(MAC_GIT_EVERY);
            waited += MAC_GIT_EVERY.as_millis() as u64;
        }
    }
}
