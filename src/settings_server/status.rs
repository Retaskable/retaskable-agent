//! The settings page's Now tab (agent-settings.md section 4.5): `GET /api/settings/status`, a
//! read-only snapshot of the computer's lanes and engines, ported from `agent/src/computerStatus.ts`
//! and `shared/computerStatus.ts`. Names and numbers only. An engine's limit is "Not reported".
//! Behind the `live-sessions-reporting` flag, each lane shows the session on it from the live
//! registry (`orchestrator::live::sessions`), the `sessions` list holds every session with no lane,
//! and lanes busy counts lanes with a session; Off, lanes the orchestrator loop holds
//! (`orchestrator::live`) show busy, a project's spec draft is the one lane-less row, and the rest
//! are free. A session row holds names and times, never a prompt or transcript. The shape is the same as the TypeScript
//! agent's so the page reads either, plus an `orchestrator` block for the loop's mode and decisions.
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use super::SettingsServer;

use crate::config::{self, Engine, EngineChoiceConfig, LocalModelConfig};
use crate::engines::choose::{self, Role};
use crate::orchestrator::driver::OrchestratorMode;
use crate::orchestrator::live::sessions::SessionRow as SessionRowLive;
use crate::orchestrator::live::{LiveHandle, LiveState};
use crate::setup_check::{SetupCheck, SetupChecks};
use crate::store::MachineStore;
use crate::view::AgentView;

/// What the status is built from beyond the config files: the agent's own view of itself, when it
/// started, and the latest setup checks (none until the first run finishes).
pub struct StatusSource {
    pub view: AgentView,
    pub started: Instant,
    pub checks: Arc<Mutex<Option<SetupChecks>>>,
    /// What the orchestrator loop is doing; off and empty when it isn't running.
    pub live: LiveHandle,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub name: String,
    pub state: &'static str,
    pub state_text: &'static str,
    pub version: String,
    pub uptime_sec: u64,
    pub projects: usize,
    pub lanes_busy: usize,
    pub lanes_total: usize,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Alert {
    pub tone: &'static str,
    pub title: String,
    pub detail: String,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Lane {
    pub k: u32,
    pub engine: String,
    pub own: bool,
    pub state: &'static str,
    pub state_text: &'static str,
    pub note: String,
    /// The item holding it, when the orchestrator loop does.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item: Option<LaneItem>,
    /// The session on it, with live sessions on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionRow>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct LaneItem {
    pub id: String,
    pub title: String,
    pub project: String,
}

/// A running session with no lane (or, on a lane, the session on it).
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionRow {
    pub id: String,
    pub project: String,
    pub title: String,
    /// Only with live sessions on: what the session does, and on what.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_sec: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiet_sec: Option<u64>,
}

impl SessionRow {
    fn drafting(id: &str, project: &str) -> Self {
        Self {
            id: id.into(),
            project: project.into(),
            title: group_word("drafter").into(),
            role: None,
            engine: None,
            model: None,
            run_sec: None,
            quiet_sec: None,
        }
    }

    fn live(r: &SessionRowLive) -> Self {
        let i = &r.info;
        Self {
            id: i.item_id.clone(),
            project: i.project.clone(),
            title: i.title.clone().unwrap_or_else(|| i.item_id.clone()),
            role: Some(format!("{:?}", i.role).to_lowercase()),
            engine: Some(i.engine.to_string()),
            model: Some(i.model.clone()),
            run_sec: Some(r.started.elapsed().as_secs()),
            quiet_sec: Some(r.last_output.elapsed().as_secs()),
        }
    }
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Limit {
    pub used_pct: Option<u32>,
    pub label: &'static str,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EngineRow {
    pub id: String,
    pub name: &'static str,
    pub in_use: bool,
    pub found: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Claude Code only: `Some(None)` when it couldn't be told.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signed_in: Option<Option<bool>>,
    pub state: &'static str,
    pub state_text: &'static str,
    pub used_for: Vec<String>,
    pub running: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<Limit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub today_tokens: Option<u64>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ComputerStatus {
    pub header: Header,
    pub alerts: Vec<Alert>,
    pub lanes: Vec<Lane>,
    /// Sessions that hold no lane: every one with live sessions on, else a project's spec draft.
    pub sessions: Vec<SessionRow>,
    pub engines: Vec<EngineRow>,
    pub orchestrator: Orchestrator,
}

/// The orchestrator loop as the page shows it: a note for the whole computer and a line per project.
#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Orchestrator {
    pub mode: &'static str,
    /// Quiet when off; says plainly that nothing is claimed when dry.
    pub note: String,
    pub projects: Vec<OrchestratorProject>,
}

#[derive(Debug, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OrchestratorProject {
    pub project: String,
    pub paused: bool,
    pub last_tick: Option<String>,
    pub decision: Option<String>,
    /// "Chores today: 2 of 3", once the store has answered a count.
    pub chores: Option<String>,
    /// What the idle baseline last found on the base branch, when it matters.
    pub fix_main: Option<String>,
    /// Why the last main sync didn't start, when it didn't.
    pub sync_main: Option<String>,
    /// The last vet run, "12 suggestions: 2 promising, 1 duplicate, 9 not promising"; absent before a run.
    pub vet: Option<String>,
}

/// The vet pass's counts by verdict as one line; zero counts are left out.
fn vet_line(counts: &std::collections::BTreeMap<String, usize>) -> String {
    let n = |k: &str| counts.get(k).copied().unwrap_or(0);
    let total: usize = counts.values().sum();
    let parts: Vec<String> = [
        ("promising", "promising", "promising"),
        ("duplicate", "duplicate", "duplicates"),
        ("not_promising", "not promising", "not promising"),
        ("spam", "spam", "spam"),
    ]
    .iter()
    .filter(|(k, ..)| n(k) > 0)
    .map(|(k, one, many)| format!("{} {}", n(k), if n(k) == 1 { one } else { many }))
    .collect();
    format!(
        "{total} suggestion{}: {}",
        if total == 1 { "" } else { "s" },
        parts.join(", ")
    )
}

fn orchestrator_note(live: &LiveState) -> String {
    match live.mode {
        OrchestratorMode::Off => "Orchestrator off".into(),
        OrchestratorMode::Dry => "Orchestrator running dry: it reads the backlog and claims nothing".into(),
        OrchestratorMode::Claim => "Orchestrator running dry (claim isn't built yet): it claims nothing".into(),
        OrchestratorMode::Runs if live.run_kinds.is_empty() => {
            "Orchestrator running on runs: it can take no kinds of run yet, so it takes nothing".into()
        }
        OrchestratorMode::Runs => format!(
            "Orchestrator running on runs: it can take {} runs",
            live.run_kinds.join(", ")
        ),
    }
}

/// `2026-10-29T14:03:05Z` as `2026-10-29 14:03 UTC`; anything else as it came.
fn friendly_time(iso: &str) -> String {
    match (iso.get(..10), iso.get(11..16)) {
        (Some(day), Some(hm)) if iso.len() > 16 => format!("{day} {hm} UTC"),
        _ => iso.to_string(),
    }
}

/// Every engine row, in the order the page lists them (`USED_ENGINES`, then the local model).
const ENGINES: [Engine; 5] = [Engine::Claude, Engine::Cursor, Engine::Pi, Engine::Codex, Engine::Local];

fn engine_name(e: Engine) -> &'static str {
    match e {
        Engine::Claude => "Claude Code",
        Engine::Cursor => "Cursor",
        Engine::Pi => "pi",
        Engine::Codex => "Codex",
        Engine::Local => "Local model",
    }
}

fn group_word(group: &str) -> &'static str {
    match group {
        "planner" => "Planning",
        "implementer" => "Building",
        "review" => "Review",
        "drafter" => "Drafting specs",
        "chat" => "Chat",
        _ => "Writing",
    }
}

/// What the status reads from the config, apart from the agent's live view.
pub struct Settled {
    pub choice: EngineChoiceConfig,
    pub local: LocalModelConfig,
    pub lanes: u32,
    pub codex_tokens: Option<u64>,
    pub cursor_tokens: Option<u64>,
    /// Card flag `live-sessions-reporting`: show the session registry's rows.
    pub live_sessions: bool,
}

/// `computerStatus`: builds the snapshot. Pure, so it's tested without a server.
pub fn build(
    settled: &Settled,
    source: &StatusSource,
    checks: Option<&SetupChecks>,
    live: &LiveState,
) -> ComputerStatus {
    let view = &source.view;
    let choice = &settled.choice;
    let mut lanes: Vec<Lane> = (1..=settled.lanes)
        .map(|k| {
            let engine = choose::engine_for(choice, None, Role::Implementer, Some(k));
            let note = if view.chore_lane == Some(k) {
                "chore lane: playtest fixes first, then roadmap items, then chores"
            } else {
                "Nothing on it"
            };
            Lane {
                k,
                engine: if engine == Engine::Local {
                    Engine::Claude
                } else {
                    engine
                }
                .to_string(),
                own: choose::lane_engine(choice, k).is_some(),
                state: "free",
                state_text: "Free",
                note: note.to_string(),
                item: None,
                session: None,
            }
        })
        .collect();
    let mut lanes_busy = 0;
    let rows = if settled.live_sessions {
        source.live.sessions().snapshot()
    } else {
        Vec::new()
    };
    let mut lane_less = Vec::new();
    for r in &rows {
        let Some(lane) = r.info.lane.and_then(|k| lanes.iter_mut().find(|l| l.k == k)) else {
            lane_less.push(SessionRow::live(r));
            continue;
        };
        lanes_busy += 1;
        lane.state = "busy";
        lane.state_text = "Busy";
        lane.note = format!("Running for {}", r.info.project);
        lane.session = Some(SessionRow::live(r));
    }
    for (project, p) in live.projects.iter().filter(|_| !settled.live_sessions) {
        for held in &p.held {
            let Some(lane) = lanes.iter_mut().find(|l| l.k == held.lane) else {
                continue;
            };
            lanes_busy += 1;
            lane.state = "busy";
            lane.state_text = "Busy";
            lane.note = format!("Held for {project}");
            lane.item = Some(LaneItem {
                id: held.item_id.clone(),
                title: held.title.clone().unwrap_or_else(|| held.item_id.clone()),
                project: project.clone(),
            });
        }
    }

    let mut alerts = Vec::new();
    if checks.is_some_and(|c| c.claude_signed_in.ok == Some(false)) {
        alerts.push(Alert {
            tone: "bad",
            title: "Claude Code is signed out".to_string(),
            detail: "Nothing runs on Claude Code until it is signed in again. Run claude auth login on this computer."
                .to_string(),
        });
    }
    for (project, p) in &live.projects {
        if let Some(f) = &p.store_error {
            alerts.push(Alert {
                tone: "warn",
                title: format!(
                    "Can't reach the backlog for {project} since {}",
                    friendly_time(&f.since)
                ),
                detail: format!("{} Nothing is started for it until the backlog answers.", f.message),
            });
        }
    }
    let mut all_paused = !view.projects.is_empty();
    for p in &view.projects {
        match view.paused.reason(&p.id) {
            Some(reason) => alerts.push(Alert {
                tone: "warn",
                title: format!(
                    "{} is paused",
                    if p.github_repo.is_empty() {
                        &p.id
                    } else {
                        &p.github_repo
                    }
                ),
                detail: reason,
            }),
            None => all_paused = false,
        }
    }

    let engines = ENGINES.iter().map(|&e| engine_row(e, settled, checks)).collect();
    let (state, state_text) = if all_paused {
        ("paused", "Paused")
    } else {
        ("running", "Running")
    };
    ComputerStatus {
        header: Header {
            name: view.machine.clone(),
            state,
            state_text,
            version: view.version.clone(),
            uptime_sec: source.started.elapsed().as_secs(),
            projects: view.projects.len(),
            lanes_busy,
            lanes_total: lanes.len(),
        },
        alerts,
        lanes,
        sessions: if settled.live_sessions {
            lane_less
        } else {
            (live.projects.iter())
                .filter_map(|(project, p)| p.drafting.as_ref().map(|id| SessionRow::drafting(id, project)))
                .collect()
        },
        engines,
        orchestrator: Orchestrator {
            mode: live.mode.name(),
            note: orchestrator_note(live),
            projects: live
                .projects
                .iter()
                .map(|(project, p)| OrchestratorProject {
                    project: project.clone(),
                    paused: p.paused,
                    last_tick: p.last_tick.clone(),
                    decision: p.decision.clone(),
                    fix_main: p.fix_main.clone(),
                    sync_main: p.sync_main.clone(),
                    vet: p.vet.as_ref().map(vet_line),
                    chores: p.chores.map(|c| format!("Chores today: {} of {}", c.today, c.limit)),
                })
                .collect(),
        },
    }
}

fn engine_row(e: Engine, settled: &Settled, checks: Option<&SetupChecks>) -> EngineRow {
    let local = &settled.local;
    let (groups, own) = choose::used_for(&settled.choice, None, e, settled.lanes);
    let used_for: Vec<String> = groups
        .iter()
        .map(|g| group_word(g).to_string())
        .chain(own.iter().map(|k| format!("Lane {k}")))
        .collect();
    let in_use = !used_for.is_empty() || (e == Engine::Local && local.enabled);
    let check: Option<&SetupCheck> = match (e, checks) {
        (Engine::Claude, Some(c)) => Some(&c.claude),
        (Engine::Cursor, Some(c)) => Some(&c.cursor),
        (Engine::Pi, Some(c)) => c.pi.as_ref(),
        (Engine::Codex, Some(c)) => c.codex.as_ref(),
        _ => None,
    };
    // Local: this agent has no probe for the server, so enabled reads as "not known".
    let found = match e {
        Engine::Local => (!local.enabled).then_some(false),
        Engine::Pi | Engine::Codex => checks.map(|_| check.is_some()),
        _ => check.map(|c| c.ok == Some(true) || !c.detail.contains("isn't installed")),
    };
    let signed_in = (e == Engine::Claude).then(|| checks.and_then(|c| c.claude_signed_in.ok));
    let version = match e {
        Engine::Local => local.enabled.then(|| local.model.clone()).filter(|m| !m.is_empty()),
        _ => check.map(|c| c.detail.clone()),
    };
    let (mut state, mut state_text) = ("ok", "Ready");
    if signed_in == Some(Some(false)) {
        (state, state_text) = ("problem", "Signed out");
    } else if found == Some(false) {
        (state, state_text) = if in_use {
            ("problem", "Not installed")
        } else {
            ("off", if e == Engine::Local { "Off" } else { "Not installed" })
        };
    } else if signed_in == Some(Some(true)) {
        state_text = "Signed in";
    } else if e == Engine::Cursor && check.is_some_and(|c| c.ok == Some(false)) {
        (state, state_text) = ("problem", "Not signed in");
    }
    EngineRow {
        id: e.to_string(),
        name: engine_name(e),
        in_use,
        found,
        version,
        signed_in,
        state,
        state_text,
        used_for,
        running: 0,
        limit: (e != Engine::Local).then_some(Limit {
            used_pct: None,
            label: "Not reported",
        }),
        today_tokens: match e {
            Engine::Codex => settled.codex_tokens,
            Engine::Cursor => settled.cursor_tokens,
            _ => None,
        },
    }
}

/// `GET /api/settings/status`: same sign-in as the settings themselves.
pub(super) async fn get_status(State(settings): State<Arc<SettingsServer>>, headers: HeaderMap) -> Response {
    if settings.authenticate(&headers).is_none() {
        return super::unauthorized("Sign in first.");
    }
    match settings.current_status() {
        Some(Ok(status)) => Json(status).into_response(),
        Some(Err(e)) => super::internal_error(&e.to_string()),
        None => super::internal_error("The agent hasn't finished starting."),
    }
}

impl SettingsServer {
    /// The Now tab's snapshot, from the config on disk now and the agent's live view. `None`
    /// before the agent has given this server one (a test server, or the first moments of a start).
    pub(super) fn current_status(&self) -> Option<Result<ComputerStatus>> {
        let source = self.status.get()?;
        Some((|| {
            let env = &self.env;
            let cfg = config::load(&self.dir)?;
            let machine = MachineStore::load(&self.dir)?;
            let settled = Settled {
                choice: config::engine_choice(&cfg, env),
                local: config::local_model(&cfg, env),
                lanes: config::lanes(&cfg, env),
                codex_tokens: machine.codex_today_tokens(),
                cursor_tokens: machine.cursor_today_tokens(),
                live_sessions: self.live_sessions_flag.load(std::sync::atomic::Ordering::Relaxed),
            };
            let checks = source.checks.lock().unwrap().clone();
            Ok(build(&settled, source, checks.as_ref(), &source.live.snapshot()))
        })())
    }

    /// Hands the server what the Now tab reads beyond the config files. Set once, at start.
    pub fn set_status_source(&self, source: StatusSource) {
        let _ = self.status.set(source);
    }
}

#[cfg(test)]
mod tests;
