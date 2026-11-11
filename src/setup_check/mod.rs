//! docs/specs/first-run-check.md section 3.1: the checks that tell a person what's missing
//! before their first session, ported from `agent/src/setupCheck.ts` and
//! `agent/src/engines/probe.ts`. This agent has no Node runtime to check, so `node` isn't
//! ported; everything else (`git`, `ghInstalled`, `gh`, `claude`, `claudeSignedIn`, `cursor`) is.
use crate::env::Env;
use std::path::Path;
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::Value;
use tokio::process::Command;
use tokio::time::timeout;

use crate::engines::resolve::{
    codex_found, pi_found, resolve_claude_on_path, resolve_codex_on_path, resolve_cursor_on_path, resolve_pi_on_path,
};

pub mod runner;
pub use runner::SetupCheckRunner;

const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

mod fixes {
    pub const GIT: &str = "Install git from git-scm.com.";
    pub const GH_MISSING: &str = "Install the GitHub CLI from cli.github.com, then run `gh auth login`.";
    pub const GH: &str = "Run `gh auth login` in a terminal on this computer.";
    pub const CLAUDE: &str = "Install Claude Code: `npm install -g @anthropic-ai/claude-code`.";
    pub const CLAUDE_SIGNED_IN: &str = "Run `claude` in a terminal on this computer and sign in.";
    pub const CURSOR_MISSING: &str = "Install Cursor's agent from cursor.com/cli, then run `cursor-agent login`.";
    pub const DOCKER: &str = "Start Docker (or install it) on this computer.";
    pub const CURSOR: &str = "Run `cursor-agent login`.";
    pub const PI: &str = "Install pi from pi.dev.";
    pub const CODEX: &str = "Install Codex: `npm install -g @openai/codex`, then run `codex login`.";
    pub const CODEX_SIGNED_IN: &str = "Run `codex login`.";
}

/// One check: `ok: None` means "couldn't tell", matching `SetupCheck` in `shared/types.ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetupCheck {
    pub ok: Option<bool>,
    pub detail: String,
    pub fix: Option<String>,
}

impl SetupCheck {
    pub(crate) fn pass(detail: impl Into<String>) -> Self {
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

    pub(crate) fn unknown(detail: impl Into<String>, fix: Option<&str>) -> Self {
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
    /// `pi --version`, present only when the binary ran; never counted by `setup_checks_pass`.
    pub pi: Option<SetupCheck>,
    /// `codex --version`, same rules as `pi`.
    pub codex: Option<SetupCheck>,
    /// Docker for agent CI (agent-ci.md section 3.6); `None` unless the `agent-ci` flag is On.
    pub docker: Option<SetupCheck>,
}

impl SetupCheck {
    fn to_json(&self) -> Value {
        let mut v = serde_json::json!({ "ok": self.ok, "detail": self.detail });
        if let Some(fix) = &self.fix {
            v["fix"] = Value::String(fix.clone());
        }
        v
    }
}

impl SetupChecks {
    /// Every check unknown ("not checked"), for a runner built without a real probe.
    pub(crate) fn all_unknown() -> Self {
        let unknown = || SetupCheck::unknown("not checked", None);
        Self {
            git: unknown(),
            gh_installed: unknown(),
            gh: unknown(),
            claude: unknown(),
            claude_signed_in: unknown(),
            cursor: unknown(),
            pi: None,
            codex: None,
            docker: None,
        }
    }

    /// `SetupChecks` in `shared/types.ts`. `node` is a stand-in: the hub's schema asks for one.
    pub fn to_json(&self) -> Value {
        let mut v = serde_json::json!({
            "node": { "ok": true, "detail": "Not needed: this agent is a compiled program" },
            "git": self.git.to_json(),
            "ghInstalled": self.gh_installed.to_json(),
            "gh": self.gh.to_json(),
            "claude": self.claude.to_json(),
            "claudeSignedIn": self.claude_signed_in.to_json(),
            "cursor": self.cursor.to_json(),
        });
        for (key, check) in [("pi", &self.pi), ("codex", &self.codex), ("docker", &self.docker)] {
            if let Some(c) = check {
                v[key] = c.to_json();
            }
        }
        v
    }
}

struct RunOutput {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// `firstLine` in `shared/text.ts` / `agent/src/engines/probe.ts`, cut to 200 chars.
fn first_line(s: &str) -> String {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    crate::text::truncate(line, 200)
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

/// pi's probe (engines/09-setup.md section 9.1): not installed, a `--version` that failed (its
/// stderr's first line), or a pass naming the first line of `--version`. pi has no sign-in check.
fn pi_check(version: Option<&RunOutput>) -> SetupCheck {
    version_check("pi", version, fixes::PI)
}

/// Codex's probe: pi's three states, then `codex login status` after a good `--version`.
fn codex_check(version: Option<&RunOutput>, login: Option<&RunOutput>) -> SetupCheck {
    let check = version_check("Codex", version, fixes::CODEX);
    if check.ok == Some(true) && !matches!(login, Some(r) if r.code == Some(0)) {
        return SetupCheck::fail("Codex isn't signed in", fixes::CODEX_SIGNED_IN);
    }
    check
}

fn version_check(name: &str, r: Option<&RunOutput>, fix: &str) -> SetupCheck {
    let Some(r) = r else {
        return SetupCheck::fail(format!("{name} isn't installed"), fix);
    };
    if r.code != Some(0) {
        return SetupCheck::fail(first_line(&r.stderr), fix);
    }
    let line = first_line(&r.stdout);
    SetupCheck::pass(if line.is_empty() { name.to_string() } else { line })
}

/// Before the `engine-setup-probes` flag: a pass naming the first line of `--version`, only when the binary ran.
fn plain_engine_check(name: &str, r: Option<&RunOutput>) -> Option<SetupCheck> {
    r.filter(|r| r.code == Some(0))
        .map(|r| version_check(name, Some(r), ""))
}

/// `runSetupChecks()`: runs every probe, never through a shell, the git/`gh`/engine probes
/// concurrently the same way the TypeScript agent does so a missing binary's 20s timeout doesn't
/// stack with the others.
pub async fn run_setup_checks(env: &Env, probes: bool) -> SetupChecks {
    let claude_bin = resolve_claude_on_path(env).ok();
    let cursor_bin = resolve_cursor_on_path(env);
    let api_key = env.get("ANTHROPIC_API_KEY").is_some();

    let pi_bin = resolve_pi_on_path(env);
    let codex_bin = resolve_codex_on_path(env);

    let (git, gh_version, gh_auth, claude_version, cursor_version, pi_version, codex_version) = tokio::join!(
        run("git", &["--version"]),
        run("gh", &["--version"]),
        run("gh", &["auth", "status"]),
        run_opt(claude_bin.as_deref(), &["--version"]),
        run(&cursor_bin, &["--version"]),
        run(&pi_bin, &["--version"]),
        run(&codex_bin, &["--version"]),
    );
    let git = git_check(git.as_ref());
    let claude = claude_check(claude_version.as_ref());
    let claude_ok = claude.ok == Some(true);
    let cursor_installed = matches!(&cursor_version, Some(r) if r.code == Some(0));

    let codex_ok = matches!(&codex_version, Some(r) if r.code == Some(0));
    let (claude_auth, cursor_status, codex_login) = tokio::join!(
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
        async {
            if probes && codex_ok {
                run(&codex_bin, &["login", "status"]).await
            } else {
                None
            }
        },
    );
    let dirs = crate::env_path::path_dirs(env);
    let (pi, codex) = if probes {
        (
            pi_found(env, cfg!(windows), &dirs).then(|| pi_check(pi_version.as_ref())),
            codex_found(env, cfg!(windows), &dirs).then(|| codex_check(codex_version.as_ref(), codex_login.as_ref())),
        )
    } else {
        (
            plain_engine_check("pi", pi_version.as_ref()),
            plain_engine_check("Codex", codex_version.as_ref()),
        )
    };

    SetupChecks {
        git,
        gh_installed: gh_installed_check(gh_version.as_ref()),
        gh: gh_check(gh_auth.as_ref()),
        claude,
        claude_signed_in: claude_signed_in_check(claude_ok, claude_auth.as_ref(), api_key),
        cursor: cursor_check(cursor_installed, cursor_status.as_ref()),
        pi,
        codex,
        docker: None,
    }
}

/// The Docker line (agent-ci.md section 3.6): `docker version` answers and, given the project's image, it runs.
pub async fn docker_check(program: impl AsRef<std::ffi::OsStr>, in_image: bool, image: Option<&str>) -> SetupCheck {
    use crate::agent_ci::docker::{DockerState, probe, probe_image};
    let program = program.as_ref();
    let mut state = probe(program, in_image).await;
    if let (DockerState::Ok, Some(image)) = (&state, image) {
        state = probe_image(program, image).await;
    }
    match state {
        DockerState::Ok => SetupCheck::pass("Docker is running"),
        DockerState::Missing(why) | DockerState::Stopped(why) => SetupCheck::fail(why, fixes::DOCKER),
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

/// `setupCheckLines`: git, gh, Claude Code, Claude Code's sign-in, Cursor only when shown, pi and Codex only when
/// found and `probes` (the `engine-setup-probes` flag) is On, and Docker only with the `agent-ci` flag On.
/// `ghInstalled` is a separate check, not one of these lines.
pub fn setup_check_lines(c: &SetupChecks, probes: bool) -> Vec<String> {
    let mut shown = vec![&c.git, &c.gh, &c.claude, &c.claude_signed_in];
    if show_cursor(c) {
        shown.push(&c.cursor);
    }
    let mut lines: Vec<String> = shown.into_iter().map(setup_check_line).collect();
    if probes {
        for (name, check) in [("pi", &c.pi), ("Codex", &c.codex)] {
            let Some(check) = check else { continue };
            lines.push(match check.ok {
                // A pass names the engine before its version, as Cursor's does; the check keeps the raw version.
                Some(true) => format!("✓ {name}: {}", check.detail),
                _ => setup_check_line(check),
            });
        }
    }
    lines.extend(c.docker.as_ref().map(setup_check_line));
    lines
}

/// `setupCheckReport`: "Setup check:" followed by each line, indented.
pub fn setup_check_report(c: &SetupChecks, probes: bool) -> String {
    let mut lines = vec!["Setup check:".to_string()];
    lines.extend(setup_check_lines(c, probes).into_iter().map(|l| format!("  {l}")));
    lines.join("\n")
}

#[cfg(test)]
mod tests;
