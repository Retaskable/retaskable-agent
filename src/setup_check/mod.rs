//! docs/specs/first-run-check.md section 3.1: the checks that tell a person what's missing
//! before their first session, ported from `agent/src/setupCheck.ts` and
//! `agent/src/engines/probe.ts`. This agent has no Node runtime to check, so `node` isn't
//! ported; everything else (`git`, `ghInstalled`, `gh`, `claude`, `claudeSignedIn`, `cursor`) is.
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

use crate::engines::resolve_claude_on_path;

const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

mod fixes {
    pub const GIT: &str = "Install git from git-scm.com.";
    pub const GH_MISSING: &str = "Install the GitHub CLI from cli.github.com, then run `gh auth login`.";
    pub const GH: &str = "Run `gh auth login` in a terminal on this computer.";
    pub const CLAUDE: &str = "Install Claude Code: `npm install -g @anthropic-ai/claude-code`.";
    pub const CLAUDE_SIGNED_IN: &str = "Run `claude` in a terminal on this computer and sign in.";
    pub const CURSOR_MISSING: &str = "Install Cursor's agent from cursor.com/cli, then run `cursor-agent login`.";
    pub const CURSOR: &str = "Run `cursor-agent login`.";
}

/// One check: `ok: None` means "couldn't tell", matching `SetupCheck` in `shared/types.ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupCheck {
    pub ok: Option<bool>,
    pub detail: String,
    pub fix: Option<String>,
}

impl SetupCheck {
    fn pass(detail: impl Into<String>) -> Self {
        Self {
            ok: Some(true),
            detail: detail.into(),
            fix: None,
        }
    }

    fn fail(detail: impl Into<String>, fix: &str) -> Self {
        Self {
            ok: Some(false),
            detail: detail.into(),
            fix: Some(fix.to_string()),
        }
    }

    fn unknown(detail: impl Into<String>, fix: Option<&str>) -> Self {
        Self {
            ok: None,
            detail: detail.into(),
            fix: fix.map(str::to_string),
        }
    }
}

/// `SetupChecks` in `shared/types.ts`, minus `node`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupChecks {
    pub git: SetupCheck,
    pub gh_installed: SetupCheck,
    pub gh: SetupCheck,
    pub claude: SetupCheck,
    pub claude_signed_in: SetupCheck,
    pub cursor: SetupCheck,
}

struct RunOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// `firstLine` in `shared/text.ts` / `agent/src/engines/probe.ts`, cut to 200 chars.
fn first_line(s: &str) -> String {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    line.chars().take(200).collect()
}

/// `run` (`agent/src/proc.ts`): never through a shell, timed out and killed after `timeout`.
async fn run_timeout(bin: impl AsRef<Path>, args: &[&str], budget: Duration) -> Option<RunOutput> {
    let mut cmd = Command::new(bin.as_ref());
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn().ok()?;
    let output = timeout(budget, child.wait_with_output()).await.ok()?.ok()?;
    Some(RunOutput {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

async fn run(bin: impl AsRef<Path>, args: &[&str]) -> Option<RunOutput> {
    run_timeout(bin, args, CHECK_TIMEOUT).await
}

async fn run_opt(bin: Option<&Path>, args: &[&str]) -> Option<RunOutput> {
    match bin {
        Some(bin) => run(bin, args).await,
        None => None,
    }
}

/// `resolveCursor()` (`agent/src/engines/cursor.ts`): `RETASKABLE_CURSOR` always wins; otherwise
/// the first `cursor-agent[.exe]` (or, on Windows, `cursor-agent.cmd`) on PATH, then
/// `~/.local/bin`. Never errors (unlike `resolve_claude`): a missing Cursor isn't fatal, so this
/// falls back to the bare name and lets the spawn itself report "not installed".
pub fn resolve_cursor(
    env: impl Fn(&str) -> Option<String>,
    windows: bool,
    path_dirs: &[PathBuf],
    home: Option<PathBuf>,
) -> PathBuf {
    if let Some(over) = env("RETASKABLE_CURSOR") {
        return PathBuf::from(over);
    }
    let name = if windows { "cursor-agent.exe" } else { "cursor-agent" };
    for dir in path_dirs {
        let exe = dir.join(name);
        if exe.is_file() {
            return exe;
        }
        if windows {
            let shim = dir.join("cursor-agent.cmd");
            if shim.is_file() {
                return shim;
            }
        }
    }
    if let Some(home) = home {
        let fallback = home.join(".local").join("bin").join(name);
        if fallback.is_file() {
            return fallback;
        }
    }
    PathBuf::from(name)
}

/// `resolve_cursor`, reading PATH and the home directory itself the way the real agent does.
pub fn resolve_cursor_on_path(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let path = env("PATH").unwrap_or_default();
    let dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
    let home = env("HOME").or_else(|| env("USERPROFILE")).map(PathBuf::from);
    resolve_cursor(&env, cfg!(windows), &dirs, home)
}

fn git_check(r: Option<&RunOutput>) -> SetupCheck {
    match r {
        Some(r) if r.code == Some(0) => {
            let line = first_line(&r.stdout);
            SetupCheck::pass(if line.is_empty() { "git".to_string() } else { line })
        }
        _ => SetupCheck::fail("git isn't installed", fixes::GIT),
    }
}

fn gh_installed_check(r: Option<&RunOutput>) -> SetupCheck {
    match r {
        Some(r) if r.code == Some(0) => {
            let line = first_line(&r.stdout);
            SetupCheck::pass(if line.is_empty() {
                "The GitHub CLI".to_string()
            } else {
                line
            })
        }
        _ => SetupCheck::fail("The GitHub CLI isn't installed", fixes::GH_MISSING),
    }
}

fn gh_check(auth: Option<&RunOutput>) -> SetupCheck {
    match auth {
        None => SetupCheck::fail("The GitHub CLI isn't installed", fixes::GH_MISSING),
        Some(r) if r.code == Some(0) => SetupCheck::pass("gh is signed in"),
        Some(_) => SetupCheck::fail("gh isn't signed in", fixes::GH),
    }
}

static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d+\.\d+\.\d+").unwrap());

fn claude_check(r: Option<&RunOutput>) -> SetupCheck {
    match r {
        Some(r) if r.code == Some(0) => {
            let line = first_line(&r.stdout);
            let detail = match VERSION_RE.find(&line) {
                Some(m) => format!("Claude Code {}", m.as_str()),
                None => "Claude Code".to_string(),
            };
            SetupCheck::pass(detail)
        }
        _ => SetupCheck::fail("Claude Code isn't installed", fixes::CLAUDE),
    }
}

static NOT_SIGNED_IN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)not (logged|signed) in|please (log|sign) in").unwrap());
static UNKNOWN_USAGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)unknown|usage").unwrap());

fn claude_signed_in_check(installed: bool, r: Option<&RunOutput>, api_key: bool) -> SetupCheck {
    if api_key {
        return SetupCheck::pass("Claude Code uses ANTHROPIC_API_KEY");
    }
    if !installed {
        return SetupCheck::unknown("Checked once Claude Code is installed", None);
    }
    let Some(r) = r else {
        return SetupCheck::unknown(
            "Couldn't tell whether Claude Code is signed in; the trial change will show it",
            Some(fixes::CLAUDE_SIGNED_IN),
        );
    };
    let out = format!("{}\n{}", r.stdout, r.stderr);
    let logged_in = serde_json::from_str::<Value>(&r.stdout)
        .ok()
        .and_then(|v| v.get("loggedIn").and_then(Value::as_bool));
    if logged_in == Some(false) || NOT_SIGNED_IN_RE.is_match(&out) {
        return SetupCheck::fail("Claude Code isn't signed in", fixes::CLAUDE_SIGNED_IN);
    }
    if r.code == Some(0) && (logged_in == Some(true) || (logged_in.is_none() && !UNKNOWN_USAGE_RE.is_match(&out))) {
        return SetupCheck::pass("Claude Code is signed in");
    }
    SetupCheck::unknown(
        "Couldn't tell whether Claude Code is signed in; the trial change will show it",
        Some(fixes::CLAUDE_SIGNED_IN),
    )
}

/// `cursorCheck(probe)`: `installed` stands in for the TypeScript probe's own `--version` call.
fn cursor_check(installed: bool, status: Option<&RunOutput>) -> SetupCheck {
    if !installed {
        return SetupCheck::fail("Cursor's agent isn't installed", fixes::CURSOR_MISSING);
    }
    match status {
        Some(r) if r.code == Some(0) => {
            let line = first_line(&r.stdout);
            let detail = if line.is_empty() { "Signed in".to_string() } else { line };
            let detail = detail.strip_prefix('✓').map(str::trim_start).unwrap_or(&detail);
            SetupCheck::pass(format!("Cursor's agent: {detail}"))
        }
        _ => SetupCheck::fail("Cursor's agent isn't signed in", fixes::CURSOR),
    }
}

/// `runSetupChecks()`: runs every probe, never through a shell, the git/`gh`/engine probes
/// concurrently the same way the TypeScript agent does so a missing binary's 20s timeout doesn't
/// stack with the others.
pub async fn run_setup_checks(env: impl Fn(&str) -> Option<String>) -> SetupChecks {
    let claude_bin = resolve_claude_on_path(&env).ok();
    let cursor_bin = resolve_cursor_on_path(&env);
    let api_key = env("ANTHROPIC_API_KEY").is_some();

    let (git, gh_version, gh_auth, claude_version, cursor_version) = tokio::join!(
        run("git", &["--version"]),
        run("gh", &["--version"]),
        run("gh", &["auth", "status"]),
        run_opt(claude_bin.as_deref(), &["--version"]),
        run(&cursor_bin, &["--version"]),
    );
    let git = git_check(git.as_ref());
    let claude = claude_check(claude_version.as_ref());
    let claude_ok = claude.ok == Some(true);
    let cursor_installed = matches!(&cursor_version, Some(r) if r.code == Some(0));

    let (claude_auth, cursor_status) = tokio::join!(
        async {
            if claude_ok && !api_key {
                run_opt(claude_bin.as_deref(), &["auth", "status"]).await
            } else {
                None
            }
        },
        async {
            if cursor_installed {
                run(&cursor_bin, &["status"]).await
            } else {
                None
            }
        },
    );

    SetupChecks {
        git,
        gh_installed: gh_installed_check(gh_version.as_ref()),
        gh: gh_check(gh_auth.as_ref()),
        claude,
        claude_signed_in: claude_signed_in_check(claude_ok, claude_auth.as_ref(), api_key),
        cursor: cursor_check(cursor_installed, cursor_status.as_ref()),
    }
}

fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `claudeReady` in `shared/setupCheckText.ts`: installed, and not known to be signed out.
pub fn claude_ready(c: &SetupChecks) -> bool {
    c.claude.ok == Some(true) && c.claude_signed_in.ok != Some(false)
}

/// `engineReady`: Claude Code ready, or Cursor's agent signed in.
pub fn engine_ready(c: &SetupChecks) -> bool {
    claude_ready(c) || c.cursor.ok == Some(true)
}

/// `showCursor`: only shown once Claude Code isn't the ready default.
pub fn show_cursor(c: &SetupChecks) -> bool {
    !claude_ready(c)
}

/// `setupChecksPass`: git, a signed-in `gh` and a ready engine. Couldn't-tell isn't a failure.
pub fn setup_checks_pass(c: &SetupChecks) -> bool {
    c.git.ok == Some(true) && c.gh.ok == Some(true) && engine_ready(c)
}

/// `setupCheckLine`: "✓ detail", "✗ detail: fix" (the fix's first letter lowered), "? detail".
pub fn setup_check_line(check: &SetupCheck) -> String {
    match check.ok {
        Some(true) => format!("✓ {}", check.detail),
        None => format!("? {}", check.detail),
        Some(false) => match &check.fix {
            Some(fix) => format!("✗ {}: {}", check.detail, lower_first(fix)),
            None => format!("✗ {}", check.detail),
        },
    }
}

/// `setupCheckLines`: git, gh, Claude Code, Claude Code's sign-in, and Cursor only when shown.
/// `ghInstalled` is a separate check, not one of these lines.
pub fn setup_check_lines(c: &SetupChecks) -> Vec<String> {
    let mut shown = vec![&c.git, &c.gh, &c.claude, &c.claude_signed_in];
    if show_cursor(c) {
        shown.push(&c.cursor);
    }
    shown.into_iter().map(setup_check_line).collect()
}

/// `setupCheckReport`: "Setup check:" followed by each line, indented.
pub fn setup_check_report(c: &SetupChecks) -> String {
    let mut lines = vec!["Setup check:".to_string()];
    lines.extend(setup_check_lines(c).into_iter().map(|l| format!("  {l}")));
    lines.join("\n")
}

#[cfg(test)]
mod tests;
