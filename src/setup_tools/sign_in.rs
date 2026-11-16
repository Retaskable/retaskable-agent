//! Sign in to GitHub and to Claude from /start (setup-in-app.md section 6), once the `tool_sign_in` command arrives. The
//! agent runs the tool's own sign-in, an argument list with no shell, and puts the one-time code or link it prints in
//! hello's `setup`, so the page shows it; the person finishes in the browser. A code Claude Code asks to have pasted back
//! is typed into the OS's own prompt at the computer, never on the website.
use std::io::{Read, Write};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{Last, Running, Setup, run_program, sign_in_name};
use crate::env::Env;

/// A sign-in that hasn't finished by then is stopped (section 6.4).
const SIGN_IN: Duration = Duration::from_secs(10 * 60);
pub const GITHUB_DEVICE_PAGE: &str = "https://github.com/login/device";
const GH_LOGIN: [&str; 7] = [
    "auth",
    "login",
    "--hostname",
    "github.com",
    "--git-protocol",
    "https",
    "--web",
];
/// What the page shows while the prompt at the computer waits for Claude's code.
pub const PASTE_AT_COMPUTER: &str = "Paste the code into the box on your computer.";
pub const TIMED_OUT: &str = "The sign-in timed out. Press Sign in to start again.";

/// `text` without ANSI escape sequences.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            while let Some(&n) = chars.peek() {
                chars.next();
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// gh's "First copy your one-time code: ABCD-1234".
pub fn device_code(text: &str) -> Option<String> {
    let text = plain(text);
    let at = text.find("one-time code:")? + "one-time code:".len();
    let word: String = text[at..].trim_start().chars().take(9).collect();
    let (a, b) = word.split_once('-')?;
    let ok = |s: &str| s.len() == 4 && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
    (ok(a) && ok(b)).then_some(word)
}

/// The first sign-in link Claude Code prints, only on Anthropic's own hosts, so the page never links anywhere else.
pub fn claude_link(text: &str) -> Option<String> {
    let text = plain(text);
    for (at, _) in text.match_indices("https://") {
        let href: String = text[at..]
            .chars()
            .take_while(|c| !c.is_whitespace() && !"\"'<>".contains(*c))
            .collect();
        let host = href["https://".len()..].split(['/', '?', '#']).next().unwrap_or("");
        let host = host.split(':').next().unwrap_or("");
        if ["claude.ai", "claude.com", "anthropic.com"]
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{h}")))
        {
            return Some(href);
        }
    }
    None
}

/// `/paste (the |your )?(\w+ )?code/i`: Claude Code asking to have the code pasted back.
pub(super) fn asks_for_code(text: &str) -> bool {
    let t = plain(text).to_lowercase();
    t.match_indices("paste ").any(|(at, _)| {
        let mut rest = &t[at + "paste ".len()..];
        for article in ["the ", "your "] {
            if let Some(r) = rest.strip_prefix(article) {
                rest = r;
                break;
            }
        }
        let word = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').count();
        rest.starts_with("code") || (word > 0 && rest[word..].starts_with(" code"))
    })
}

/// The OS's own prompt at the computer, for a code Claude Code asks to have pasted back: what's typed goes only to the
/// caller, which hands it to Claude Code's stdin. `None` when it's cancelled or there's no prompt to show.
pub fn ask_at_computer() -> Option<String> {
    let text = "Paste the code from your browser to finish signing in to Claude.";
    let (cmd, args): (&str, Vec<String>) = if cfg!(target_os = "macos") {
        (
            "osascript",
            vec![
                "-e".into(),
                format!("text returned of (display dialog \"{text}\" default answer \"\" with title \"Retaskable\")"),
            ],
        )
    } else if cfg!(windows) {
        (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                format!(
                    "Add-Type -AssemblyName Microsoft.VisualBasic; [Microsoft.VisualBasic.Interaction]::InputBox('{text}', 'Retaskable')"
                ),
            ],
        )
    } else {
        (
            "zenity",
            vec!["--entry".into(), "--title=Retaskable".into(), format!("--text={text}")],
        )
    };
    let r = run_program(cmd, &args, SIGN_IN);
    let typed = r.stdout.trim();
    (r.code == Some(0) && !typed.is_empty()).then(|| typed.to_string())
}

impl Setup {
    /// Why it won't sign in to `tool` now, or `None` (section 6.1): checked when the command arrives and again as it starts.
    pub fn sign_in_refusal(&self, tool: &str, env: &Env) -> Option<String> {
        let s = self.state();
        if s.sign_in.is_some() {
            return Some("Another sign-in is running: wait for it.".into());
        }
        if s.install.is_some() {
            return Some("Another install is running: wait for it.".into());
        }
        drop(s);
        let c = self.checks.latest();
        if tool == "claude" {
            if env.get("ANTHROPIC_API_KEY").is_some() {
                return Some("This computer uses an API key.".into());
            }
            if c.as_ref().is_some_and(|c| c.claude.ok != Some(true)) {
                return Some("Install it first.".into());
            }
            return c
                .is_some_and(|c| c.claude_signed_in.ok == Some(true))
                .then(|| "Already signed in.".into());
        }
        if c.as_ref().is_some_and(|c| c.gh_installed.ok == Some(false)) {
            return Some("Install it first.".into());
        }
        c.is_some_and(|c| c.gh.ok == Some(true))
            .then(|| "Already signed in.".into())
    }

    /// Starts the tool's own sign-in. It goes on after this returns, up to 10 minutes; how it ended goes up in `hello`.
    pub fn sign_in(self: &Arc<Self>, tool: &str, env: &Env) -> Result<(), String> {
        if let Some(refusal) = self.sign_in_refusal(tool, env) {
            return Err(refusal);
        }
        let (bin, args): (String, Vec<String>) = if tool == "gh" {
            ("gh".into(), GH_LOGIN.iter().map(|s| s.to_string()).collect())
        } else {
            let bin = crate::engines::resolve::resolve_claude_on_path(env)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| "claude".into());
            (bin, vec!["auth".into(), "login".into()])
        };
        let mut cmd = crate::spawn::command(&bin);
        cmd.args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("The sign-in stopped: couldn't start {bin}: {e}"))?;
        self.state().sign_in = Some(Running {
            tool: tool.into(),
            action: "sign_in",
            since: self.now(),
            note: None,
        });
        self.changed();
        let said = Arc::new(Mutex::new(String::new()));
        let stdin = Arc::new(Mutex::new(child.stdin.take()));
        let asked = Arc::new(Mutex::new(false));
        let mut readers = Vec::new();
        let pipes: [Option<Box<dyn Read + Send>>; 2] = [
            child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
            child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        ];
        for pipe in pipes.into_iter().flatten() {
            let (me, said, stdin, asked, tool) = (
                self.clone(),
                said.clone(),
                stdin.clone(),
                asked.clone(),
                tool.to_string(),
            );
            readers.push(std::thread::spawn(move || {
                let mut pipe = pipe;
                let mut buf = [0u8; 4096];
                while let Ok(n) = pipe.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let all = {
                        let mut s = crate::sync::lock(&said);
                        s.push_str(&String::from_utf8_lossy(&buf[..n]));
                        let keep = s.len().saturating_sub(20_000);
                        let cut = (keep..=s.len()).find(|i| s.is_char_boundary(*i)).unwrap_or(0);
                        s.drain(..cut);
                        s.clone()
                    };
                    me.heard(&tool, &all, &stdin, &asked);
                }
            }));
        }
        let me = self.clone();
        let tool = tool.to_string();
        std::thread::spawn(move || {
            let start = Instant::now();
            let mut timed_out = false;
            let code = loop {
                match child.try_wait() {
                    Ok(Some(s)) => break s.code().unwrap_or(-1),
                    Ok(None) if start.elapsed() >= SIGN_IN => {
                        timed_out = true;
                        let _ = child.kill();
                        let _ = child.wait();
                        break -1;
                    }
                    Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                    Err(_) => break -1,
                }
            };
            for r in readers {
                let _ = r.join();
            }
            let text = plain(&crate::sync::lock(&said));
            me.finish_sign_in(&tool, code, timed_out, &text);
        });
        Ok(())
    }

    /// Output arrived: the one-time code or link goes up, and Claude's request for a pasted code is answered at the computer.
    fn heard(
        self: &Arc<Self>,
        tool: &str,
        said: &str,
        stdin: &Arc<Mutex<Option<std::process::ChildStdin>>>,
        asked: &Arc<Mutex<bool>>,
    ) {
        let have_note = self.state().sign_in.as_ref().is_some_and(|r| r.note.is_some());
        if !have_note {
            let note = if tool == "gh" {
                device_code(said)
            } else {
                claude_link(said)
            };
            if let Some(note) = note {
                if let Some(r) = self.state().sign_in.as_mut() {
                    r.note = Some(note);
                }
                // gh, with no terminal, prints the code and the page's address and waits: the agent opens it.
                if tool == "gh" {
                    (self.deps.open)(super::sign_in::GITHUB_DEVICE_PAGE);
                }
                self.changed();
            }
        }
        if tool == "claude" && asks_for_code(said) && !std::mem::replace(&mut *crate::sync::lock(asked), true) {
            if let Some(r) = self.state().sign_in.as_mut() {
                r.note = Some(PASTE_AT_COMPUTER.into());
            }
            self.changed();
            let (me, stdin) = (self.clone(), stdin.clone());
            std::thread::spawn(move || {
                if let Some(code) = (me.deps.ask)()
                    && let Some(w) = crate::sync::lock(&stdin).as_mut()
                {
                    let _ = writeln!(w, "{code}");
                }
            });
        }
    }

    fn finish_sign_in(&self, tool: &str, code: i32, timed_out: bool, said: &str) {
        let name = sign_in_name(tool);
        let mut ok = code == 0 && !timed_out;
        let mut detail = if timed_out {
            TIMED_OUT.to_string()
        } else if ok {
            format!("Signed in to {name}.")
        } else {
            let last = crate::proc::tail_lines(said.trim(), 1);
            crate::text::clip(
                &format!(
                    "The sign-in stopped: {}",
                    if last.is_empty() {
                        format!("exit code {code}")
                    } else {
                        last
                    }
                ),
                300,
            )
        };
        // So git pushes as the same login (section 6.2).
        if ok && tool == "gh" {
            let r = (self.deps.run)(
                "gh",
                &["auth".to_string(), "setup-git".to_string()],
                Duration::from_secs(20),
            );
            if r.code != Some(0) {
                ok = false;
                let last = crate::proc::tail_lines(r.stderr.trim(), 1);
                detail = crate::text::clip(
                    &format!(
                        "Signed in, but git couldn't be set to use it: {}",
                        if last.is_empty() {
                            "gh auth setup-git failed".to_string()
                        } else {
                            last
                        }
                    ),
                    300,
                );
            }
        }
        {
            let mut s = self.state();
            s.sign_in = None;
            s.last_sign_in = Some(Last {
                tool: tool.into(),
                action: "sign_in",
                ok,
                detail,
                at: self.now(),
            });
        }
        let _ = (self.deps.refresh)();
        self.changed();
    }
}

#[cfg(test)]
pub(super) fn asks_for_code_for_test(text: &str) -> bool {
    asks_for_code(text)
}
