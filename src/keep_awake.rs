//! Keep this computer awake (tray.md section 4.2, small-agent-jobs.md, following `agent/src/keepAwake.ts`):
//! while on, the OS does not sleep when idle, with no privileges, by a child that holds the block and
//! ends with the agent. The choice is a file in the data folder, so a restart brings it back; stopping
//! the agent lets the block go and keeps the choice. A tool that is missing or ends by itself leaves it off.
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use anyhow::Result;

/// The file whose presence is the choice.
pub const CHOICE_FILE: &str = "keep-awake";

/// The program and arguments that hold the OS awake until process `pid` ends, for `os`
/// (`std::env::consts::OS`); `None` for an OS with none.
pub fn awake_command(os: &str, pid: u32) -> Option<(String, Vec<String>)> {
    let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match os {
        "macos" => {
            let mut a = args(&["-i", "-s", "-w"]);
            a.push(pid.to_string());
            Some(("caffeinate".into(), a))
        }
        "windows" => {
            // ES_CONTINUOUS | ES_SYSTEM_REQUIRED on this thread, held while the agent runs.
            let script = format!(
                "Add-Type -MemberDefinition '[DllImport(\"kernel32.dll\")] public static extern uint \
                 SetThreadExecutionState(uint f);' -Name K -Namespace W; \
                 [void][W.K]::SetThreadExecutionState([uint32]2147483649); \
                 Wait-Process -Id {pid}"
            );
            Some((
                "powershell".into(),
                args(&[
                    "-NoProfile",
                    "-NonInteractive",
                    "-WindowStyle",
                    "Hidden",
                    "-Command",
                    &script,
                ]),
            ))
        }
        "linux" => {
            let mut a = args(&[
                "--what=idle:sleep",
                "--who=Retaskable",
                "--why=Keep this computer awake",
                "tail",
                "-f",
                "/dev/null",
                "--pid",
            ]);
            a.push(pid.to_string());
            Some(("systemd-inhibit".into(), a))
        }
        _ => None,
    }
}

pub struct KeepAwake {
    file: PathBuf,
    command: Option<(String, Vec<String>)>,
    child: Mutex<Option<Child>>,
}

impl KeepAwake {
    /// Keeps its choice in `data_dir`, holding the OS awake for this process.
    pub fn new(data_dir: &std::path::Path) -> Self {
        Self::with_command(
            data_dir.join(CHOICE_FILE),
            awake_command(std::env::consts::OS, std::process::id()),
        )
    }

    pub fn with_command(file: PathBuf, command: Option<(String, Vec<String>)>) -> Self {
        Self {
            file,
            command,
            child: Mutex::new(None),
        }
    }

    /// Whether it is on: chosen, and the tool holding it is running (after [`Self::tick`]).
    pub fn is_on(&self) -> bool {
        self.file.exists()
    }

    /// Turns it on or off and remembers the choice.
    pub fn set(&self, on: bool) -> Result<()> {
        if on {
            if let Some(parent) = self.file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&self.file, "Keep this computer awake is on.\n")?;
        } else {
            match std::fs::remove_file(&self.file) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        self.tick();
        Ok(())
    }

    /// Brings the OS in line with the choice: starts the tool when it's chosen and none runs, ends it
    /// when it isn't, and clears the choice when the tool is missing or has ended by itself. Never fails.
    pub fn tick(&self) {
        let mut child = self.child.lock().unwrap();
        if let Some(c) = child.as_mut()
            && c.try_wait().map(|s| s.is_some()).unwrap_or(true)
        {
            *child = None;
            self.forget("the tool holding the computer awake ended");
        }
        let wanted = self.file.exists();
        match (wanted, child.is_some()) {
            (true, false) => match self.spawn() {
                Some(c) => *child = Some(c),
                None => self.forget("the tool to hold the computer awake wouldn't start"),
            },
            (false, true) => {
                if let Some(mut c) = child.take() {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
            _ => {}
        }
    }

    /// Lets the block go and keeps the choice, for the agent stopping.
    pub fn stop(&self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    fn spawn(&self) -> Option<Child> {
        let (program, args) = self.command.as_ref()?;
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()
    }

    fn forget(&self, why: &str) {
        tracing::warn!("Keep awake: {why}; leaving it off.");
        let _ = std::fs::remove_file(&self.file);
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        self.stop();
    }
}
