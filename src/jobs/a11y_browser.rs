//! The accessibility sweep's real browser: a small node script (`assets/a11y-browser.cjs`, the page collectors
//! ported from the TypeScript agent's `a11yScan.ts`, `a11yPage.ts` and `a11yChecks.ts`) drives Chrome through
//! `playwright-core` and `axe-core`, and this module talks to it in line-delimited JSON ([`rpc`]). The judging
//! stays in Rust ([`super::a11y_judge`]).
//!
//! One node process lives per sweep ([`PlaywrightBrowser`]); it is killed, Chrome with it, when the browser (and
//! every [`PlaywrightWalker`]) drops.
//!
//! Where the modules come from: the script resolves `playwright-core` and `axe-core` through `NODE_PATH`, which
//! is `<RETASKABLE_A11Y_TOOLS>/node_modules` first (the Docker image bakes the pinned pair there), then
//! `<tools dir>/node_modules`, then the agent's own `NODE_PATH`. When the script says none has them, the launcher writes a `package.json` into the tools
//! dir, runs `npm install` there once, and starts again. The browser is `CHROME_PATH`, else the installed Chrome,
//! else Playwright's own Chromium (`PLAYWRIGHT_BROWSERS_PATH`, as in the Docker image); with none, the
//! [`LaunchError`] has `no_chrome`.
//!
//! Protocol (one JSON object per line, requests on stdin, answers on stdout), methods and their params:
//! `launch {base, samplePrefs}`, `pass {route, size, theme, runOnly}`, `reflow|title|spacing {route}`,
//! `walkOpen {route, sample}` (answers `{page}`), then `walkCandidates|walkEscapeTab|walkBlur {page}`,
//! `walkPress {page, key}` and `walkClose {page}`.
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

use super::a11y_judge::{Boxed, Candidate, FocusStyles, Hit, Overflow, Pressed, WalkPage, WalkStep};
use super::a11y_sweep::{BrowserLauncher, LaunchError, Pass, PassResult, RunOnly, SweepBrowser};
use crate::proc::{NPM_INSTALL_TIMEOUT, run_command_with_timeout};

mod rpc;
use rpc::{Rpc, RpcError};

/// The browser script, run as `node -e`: nothing is written to disk and nothing depends on the working directory.
const SCRIPT: &str = include_str!("../../assets/a11y-browser.cjs");
/// What the tools dir installs (the versions the TypeScript agent's `package.json` has).
/// Exact versions and no install scripts: this runs on the person's computer, so a newer release is never taken by itself.
const PACKAGE_JSON: &str = r#"{"name":"retaskable-a11y-tools","private":true,"dependencies":{"playwright-core":"1.63.0","axe-core":"4.10.3"}}"#;
/// A folder holding a ready `node_modules` with the two modules (the Docker image's `/opt/a11y-tools`); it wins
/// over the on-demand install. Its versions are pinned in the Dockerfile, which a test keeps equal to [`PACKAGE_JSON`].
pub const BAKED_TOOLS_ENV: &str = "RETASKABLE_A11Y_TOOLS";
/// Launching Chrome and opening four pages.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(120);

/// Starts a node process running the browser script.
#[derive(Debug, Clone)]
pub struct PlaywrightLauncher {
    program: OsString,
    args: Vec<OsString>,
    /// Where `playwright-core` and `axe-core` are installed to (under `node_modules`) when not found; `None`
    /// never installs.
    tools_dir: Option<PathBuf>,
}

impl PlaywrightLauncher {
    /// `node` running the embedded script, installing the two npm modules into `tools_dir` (the data dir's
    /// `a11y-tools`, say) the first time they are missing.
    pub fn new(tools_dir: impl Into<PathBuf>) -> Self {
        PlaywrightLauncher {
            program: "node".into(),
            args: vec!["-e".into(), SCRIPT.into()],
            tools_dir: Some(tools_dir.into()),
        }
    }

    /// Another program in node's place that speaks the same protocol (tests); never installs.
    pub fn with_program(program: impl Into<OsString>, args: Vec<OsString>) -> Self {
        PlaywrightLauncher {
            program: program.into(),
            args,
            tools_dir: None,
        }
    }

    fn start(&self) -> std::io::Result<Rpc> {
        let mut cmd = crate::spawn::tokio_command(&self.program);
        cmd.args(&self.args);
        if let Some(dir) = &self.tools_dir {
            let mut paths = Vec::new();
            if let Some(baked) = std::env::var_os(BAKED_TOOLS_ENV).filter(|b| !b.is_empty()) {
                paths.push(PathBuf::from(baked).join("node_modules"));
            }
            paths.push(dir.join("node_modules"));
            if let Some(own) = std::env::var_os("NODE_PATH") {
                paths.extend(std::env::split_paths(&own));
            }
            if let Ok(joined) = std::env::join_paths(paths) {
                cmd.env("NODE_PATH", joined);
            }
        }
        Rpc::spawn(cmd)
    }

    async fn launch_once(&self, base: &str, sample_prefs: Option<&Value>) -> Result<PlaywrightBrowser, RpcError> {
        let rpc = self.start().map_err(|e| RpcError {
            kind: "other".into(),
            message: format!("starting node: {e}"),
        })?;
        let params = json!({ "base": base, "samplePrefs": sample_prefs });
        rpc.call_within("launch", params, LAUNCH_TIMEOUT).await?;
        Ok(PlaywrightBrowser { rpc })
    }
}

/// `npm install` of the two modules into `dir` (blocking: run it off the runtime).
fn install_tools(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    std::fs::write(dir.join("package.json"), PACKAGE_JSON).map_err(|e| format!("writing package.json: {e}"))?;
    match run_command_with_timeout(
        "npm install --no-audit --no-fund --no-progress --ignore-scripts --save-exact",
        dir,
        NPM_INSTALL_TIMEOUT,
    ) {
        Ok((0, _, false)) => Ok(()),
        Ok((_, _, true)) => Err("npm install of playwright-core and axe-core timed out".to_string()),
        Ok((code, text, false)) => Err(format!(
            "npm install of playwright-core and axe-core exited with {code}: {}",
            crate::proc::tail_lines(text.trim(), 5)
        )),
        Err(e) => Err(format!("npm install of playwright-core and axe-core: {e:#}")),
    }
}

fn launch_error(e: RpcError) -> LaunchError {
    LaunchError {
        no_chrome: e.kind == "no_chrome",
        message: e.message,
    }
}

impl BrowserLauncher for PlaywrightLauncher {
    type Browser = PlaywrightBrowser;

    async fn launch(&self, base: &str, sample_prefs: Option<&Value>) -> Result<PlaywrightBrowser, LaunchError> {
        match self.launch_once(base, sample_prefs).await {
            Err(e) if e.kind == "modules" => {
                let Some(dir) = self.tools_dir.clone() else {
                    return Err(launch_error(e));
                };
                let installed = tokio::task::spawn_blocking(move || install_tools(&dir))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|r| r);
                if let Err(why) = installed {
                    return Err(LaunchError {
                        no_chrome: false,
                        message: format!("{} ({why})", e.message),
                    });
                }
                self.launch_once(base, sample_prefs).await.map_err(launch_error)
            }
            other => other.map_err(launch_error),
        }
    }
}

/// A launched Chrome with its four pages (phone and desktop, light and dark). The node process dies with it.
pub struct PlaywrightBrowser {
    rpc: Rpc,
}

fn parse<T: for<'de> Deserialize<'de>>(method: &str, v: Value) -> Result<T, String> {
    serde_json::from_value(v).map_err(|e| format!("{method}: unreadable answer from the browser: {e}"))
}

async fn call<T: for<'de> Deserialize<'de>>(rpc: &Rpc, method: &str, params: Value) -> Result<T, String> {
    let v = rpc.call(method, params).await.map_err(|e| e.message)?;
    parse(method, v)
}

#[derive(Deserialize)]
struct PassDto {
    passed: Vec<String>,
    failed: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReflowDto {
    scroll_width: f64,
    client_width: f64,
}

#[derive(Deserialize)]
struct OverflowDto {
    element: i64,
    role: String,
    name: String,
    ellipsis: bool,
}

impl From<OverflowDto> for Overflow {
    fn from(o: OverflowDto) -> Self {
        Overflow {
            element: o.element,
            role: o.role,
            name: o.name,
            ellipsis: o.ellipsis,
        }
    }
}

#[derive(Deserialize)]
struct SpacingDto {
    before: Vec<OverflowDto>,
    after: Vec<OverflowDto>,
}

#[derive(Deserialize)]
struct PageDto {
    page: i64,
}

#[derive(Deserialize)]
struct CandidateDto {
    element: i64,
    role: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StylesDto {
    outline_style: String,
    outline_width: String,
    outline_color: String,
    box_shadow: String,
    border_color: String,
    background_color: String,
}

impl From<StylesDto> for FocusStyles {
    fn from(s: StylesDto) -> Self {
        FocusStyles {
            outline_style: s.outline_style,
            outline_width: s.outline_width,
            outline_color: s.outline_color,
            box_shadow: s.box_shadow,
            border_color: s.border_color,
            background_color: s.background_color,
        }
    }
}

#[derive(Deserialize)]
struct BoxDto {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HitDto {
    on_element: bool,
    role: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StepDto {
    element: Option<i64>,
    role: String,
    name: String,
    #[serde(rename = "box")]
    boxed: BoxDto,
    in_viewport: bool,
    focused: StylesDto,
    hits: Vec<Option<HitDto>>,
}

#[derive(Deserialize)]
struct PressedDto {
    step: StepDto,
    prev: Option<StylesDto>,
}

impl From<PressedDto> for Pressed {
    fn from(p: PressedDto) -> Self {
        let focused: FocusStyles = p.step.focused.into();
        Pressed {
            step: WalkStep {
                element: p.step.element,
                role: p.step.role,
                name: p.step.name,
                boxed: Boxed {
                    x: p.step.boxed.x,
                    y: p.step.boxed.y,
                    width: p.step.boxed.width,
                    height: p.step.boxed.height,
                },
                in_viewport: p.step.in_viewport,
                // The walk fills in what the page says of it once the element has lost focus.
                unfocused: focused.clone(),
                focused,
                hits: p
                    .step
                    .hits
                    .into_iter()
                    .map(|h| {
                        h.map(|h| Hit {
                            on_element: h.on_element,
                            role: h.role,
                            name: h.name,
                        })
                    })
                    .collect(),
            },
            prev: p.prev.map(Into::into),
        }
    }
}

impl SweepBrowser for PlaywrightBrowser {
    type Walker = PlaywrightWalker;

    async fn run_pass(&self, route: &str, pass: &Pass) -> Result<PassResult, String> {
        let run_only = match pass.run_only {
            RunOnly::Tags(t) => json!({ "type": "tag", "values": t }),
            RunOnly::Rules(r) => json!({ "type": "rule", "values": r }),
        };
        let params =
            json!({ "route": route, "size": pass.size.name(), "theme": pass.theme.name(), "runOnly": run_only });
        let r: PassDto = call(&self.rpc, "pass", params).await?;
        Ok(PassResult {
            passed: r.passed,
            failed: r.failed,
        })
    }

    async fn walker(&self, route: &str, sample: bool) -> Result<PlaywrightWalker, String> {
        let p: PageDto = call(&self.rpc, "walkOpen", json!({ "route": route, "sample": sample })).await?;
        Ok(PlaywrightWalker {
            rpc: self.rpc.clone(),
            page: p.page,
        })
    }

    async fn reflow(&self, route: &str) -> Result<(f64, f64), String> {
        let r: ReflowDto = call(&self.rpc, "reflow", json!({ "route": route })).await?;
        Ok((r.scroll_width, r.client_width))
    }

    async fn spacing(&self, route: &str) -> Result<(Vec<Overflow>, Vec<Overflow>), String> {
        let s: SpacingDto = call(&self.rpc, "spacing", json!({ "route": route })).await?;
        Ok((
            s.before.into_iter().map(Into::into).collect(),
            s.after.into_iter().map(Into::into).collect(),
        ))
    }

    async fn title(&self, route: &str) -> Result<String, String> {
        call(&self.rpc, "title", json!({ "route": route })).await
    }
}

/// The Tab walk's page of its own; closed when it drops.
pub struct PlaywrightWalker {
    rpc: Rpc,
    page: i64,
}

impl Drop for PlaywrightWalker {
    fn drop(&mut self) {
        self.rpc.send_and_forget("walkClose", json!({ "page": self.page }));
    }
}

impl WalkPage for PlaywrightWalker {
    async fn candidates(&mut self) -> Result<Vec<Candidate>, String> {
        let c: Vec<CandidateDto> = call(&self.rpc, "walkCandidates", json!({ "page": self.page })).await?;
        Ok(c.into_iter()
            .map(|c| Candidate {
                element: c.element,
                role: c.role,
                name: c.name,
            })
            .collect())
    }

    async fn press_tab(&mut self) -> Result<Pressed, String> {
        let p: PressedDto = call(&self.rpc, "walkPress", json!({ "page": self.page, "key": "Tab" })).await?;
        Ok(p.into())
    }

    async fn press_escape_then_tab(&mut self) -> Result<Pressed, String> {
        let p: PressedDto = call(&self.rpc, "walkEscapeTab", json!({ "page": self.page })).await?;
        Ok(p.into())
    }

    async fn blur_and_read(&mut self) -> Result<Option<FocusStyles>, String> {
        let s: Option<StylesDto> = call(&self.rpc, "walkBlur", json!({ "page": self.page })).await?;
        Ok(s.map(Into::into))
    }
}

#[cfg(test)]
mod smoke;
#[cfg(test)]
mod tests;
