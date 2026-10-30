//! The settings page's Now tab (agent-settings.md section 4.5): `GET /api/settings/status`, a
//! read-only snapshot of the computer's lanes and engines, ported from `agent/src/computerStatus.ts`
//! and `shared/computerStatus.ts`. Names and numbers only. This agent has no session pool yet, so
//! every lane is free, no session runs, and an engine's limit is "Not reported"; the shape is the
//! same as the TypeScript agent's so the page reads either.
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
use crate::setup_check::{SetupCheck, SetupChecks};
use crate::store::MachineStore;
use crate::view::AgentView;

/// What the status is built from beyond the config files: the agent's own view of itself, when it
/// started, and the latest setup checks (none until the first run finishes).
pub struct StatusSource {
    pub view: AgentView,
    pub started: Instant,
    pub checks: Arc<Mutex<Option<SetupChecks>>>,
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
    /// Always empty: this agent has no session pool to list.
    pub sessions: Vec<()>,
    pub engines: Vec<EngineRow>,
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
}

/// `computerStatus`: builds the snapshot. Pure, so it's tested without a server.
pub fn build(settled: &Settled, source: &StatusSource, checks: Option<&SetupChecks>) -> ComputerStatus {
    let view = &source.view;
    let choice = &settled.choice;
    let lanes: Vec<Lane> = (1..=settled.lanes)
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
            }
        })
        .collect();

    let mut alerts = Vec::new();
    if checks.is_some_and(|c| c.claude_signed_in.ok == Some(false)) {
        alerts.push(Alert {
            tone: "bad",
            title: "Claude Code is signed out".to_string(),
            detail: "Nothing runs on Claude Code until it is signed in again. Run claude auth login on this computer."
                .to_string(),
        });
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
            lanes_busy: 0,
            lanes_total: lanes.len(),
        },
        alerts,
        lanes,
        sessions: Vec::new(),
        engines,
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
            };
            let checks = source.checks.lock().unwrap().clone();
            Ok(build(&settled, source, checks.as_ref()))
        })())
    }

    /// Hands the server what the Now tab reads beyond the config files. Set once, at start.
    pub fn set_status_source(&self, source: StatusSource) {
        let _ = self.status.set(source);
    }
}

#[cfg(test)]
mod tests;
