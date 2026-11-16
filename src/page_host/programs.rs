//! The programs panel and the other programs' settings (agent-settings.md section 4.6): `GET
//! /api/settings/programs` and `POST /api/settings/programs/{program}`. The page covers the code agent, the Chat Agent
//! and the CI runner, which are three processes that may run on different computers; this module talks to none of
//! them. It reads each one's `status.json` (`program_status`) and edits each one's own `settings.json`
//! (`program_settings`), and nothing else: there is no call to a program, no start or stop, no command.
//!
//! The code agent's own settings stay in `GET`/`POST /api/settings`; here it has only its status.
//!
//! A program's environment is not this process's. What it sets (and so what the page locks) is what its status file
//! lists in `envSet`, names only; a program that never wrote one has none known.
use std::path::Path;
use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::net::SocketAddr;

use super::auth::csrf_ok;
use super::secrets::secret_rows;
use super::{PageHost, bad_request, internal_error, json_error, not_found_response, unauthorized};
use crate::env::Env;
use crate::paths;
use crate::program_settings::{self, Refused};
use crate::program_status::{self, CHAT_AGENT, CI_RUNNER, CODE_AGENT, ProgramStatus, State as Run};
use crate::secrets::{Owner, Secrets};

/// One program the page shows.
struct Program {
    id: &'static str,
    label: &'static str,
    /// The terminal command that links it to the shared backlog; shown when it is unlinked. The page has no link
    /// button: linking is a device flow a person runs on the computer the program is on.
    link_command: &'static str,
    owner: Owner,
    /// What a save takes effect on, in words for the page.
    applies: &'static str,
}

const PROGRAMS: [Program; 3] = [
    Program {
        id: CODE_AGENT,
        label: "Code agent",
        link_command: "retaskable link backlog",
        owner: Owner::CodeAgent,
        applies: "",
    },
    Program {
        id: CHAT_AGENT,
        label: "Chat agent",
        link_command: "retaskable-chat-agent link",
        owner: Owner::ChatAgent,
        applies: "Applies when the Chat Agent next asks the store for its projects (within 5 minutes).",
    },
    Program {
        id: CI_RUNNER,
        label: "CI runner",
        link_command: "retaskable-ci link",
        owner: Owner::CiRunner,
        applies: "Applies on the CI runner's next poll (about 10 seconds); a lower slot count takes effect as running checks end.",
    },
];

fn state_json(state: &Run) -> (&'static str, Option<u64>) {
    match state {
        Run::Running => ("running", None),
        Run::Stopped(seen) => ("stopped", *seen),
        Run::Unlinked => ("unlinked", None),
        Run::NotInstalled => ("notInstalled", None),
    }
}

/// An environment holding exactly the variables a program's status file says it has set, so the shared settings
/// functions that check "does the environment set this" answer for that program rather than for this process.
fn env_of(status: Option<&ProgramStatus>) -> Env {
    Env::from_pairs(
        status
            .map(|s| s.env_set.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|name| (name, "set")),
    )
}

fn settings_view(id: &str, env: &Env, folder: &Path) -> Option<Value> {
    match id {
        CHAT_AGENT => Some(program_settings::chat::view(env, folder)),
        CI_RUNNER => Some(program_settings::ci::view(env, folder)),
        _ => None,
    }
}

impl PageHost {
    fn secrets_for(&self, owner: Owner, status: Option<&ProgramStatus>) -> Secrets {
        let os = self.program_keys.get().cloned().flatten();
        let env_set = status.map(|s| s.env_set.clone()).unwrap_or_default();
        Secrets::for_page(owner, &self.dir, &self.env, os, &env_set)
    }

    fn programs_view(&self, can_save_secrets: bool) -> Value {
        let now = (self.now)().saturating_mul(1000);
        let rows: Vec<Value> = PROGRAMS
            .iter()
            .map(|p| {
                let folder = program_folder(p.id, &self.dir);
                let status = program_status::read(&folder);
                let state = program_status::state_in(&folder, now);
                let (name, seen) = state_json(&state);
                let mut row = json!({
                    "id": p.id,
                    "label": p.label,
                    "state": name,
                    "lastSeen": seen.or(status.as_ref().map(|s| s.updated_at)),
                    "version": status.as_ref().map(|s| s.version.clone()),
                    "lastError": status.as_ref().and_then(|s| s.last_error.clone()),
                    "details": status.as_ref().map(|s| Value::Object(s.extra.clone())),
                    "linkCommand": (state == Run::Unlinked).then_some(p.link_command),
                    "settings": null,
                    "secrets": null,
                });
                // The code agent's own settings are the page's other sections; a program not on this computer has
                // nothing to edit here.
                if p.id != CODE_AGENT && state != Run::NotInstalled {
                    row["settings"] = settings_view(p.id, &env_of(status.as_ref()), &folder).unwrap_or(Value::Null);
                    let secrets = self.secrets_for(p.owner, status.as_ref());
                    row["secrets"] = Value::Array(secret_rows(&secrets));
                    row["secretsNote"] = json!(secrets.save_problem());
                    row["secretRules"] = crate::secrets::names::rules_json();
                    row["canSaveSecrets"] = json!(can_save_secrets);
                    row["applies"] = json!(p.applies);
                }
                row
            })
            .collect();
        json!({ "programs": rows })
    }

    /// A save for one program: its settings and its secrets, checked whole before either is written, then written to
    /// that program's own file (a temp file, then a rename) and no other.
    pub(crate) fn save_program(
        &self,
        id: &str,
        update: &ProgramUpdate,
        secrets_allowed: bool,
    ) -> Result<(), Box<Response>> {
        let Some(program) = PROGRAMS.iter().find(|p| p.id == id && p.id != CODE_AGENT) else {
            return Err(Box::new(not_found_response()));
        };
        let folder = program_folder(id, &self.dir);
        let now = (self.now)().saturating_mul(1000);
        if program_status::state_in(&folder, now) == Run::NotInstalled {
            return Err(Box::new(bad_request("That program isn't installed on this computer.")));
        }
        let status = program_status::read(&folder);
        let env = env_of(status.as_ref());
        let refused = |r: Refused| Box::new(bad_request(&r.0));
        let plan = match update.secrets.as_ref() {
            None => None,
            Some(_) if !secrets_allowed => {
                return Err(Box::new(json_error(
                    StatusCode::FORBIDDEN,
                    "Secrets can be saved only on this computer.",
                )));
            }
            Some(changes) => {
                let Some(changes) = changes.as_object() else {
                    return Err(Box::new(bad_request("secrets must be an object.")));
                };
                Some(
                    self.secrets_for(program.owner, status.as_ref())
                        .plan(changes)
                        .map_err(|e| Box::new(bad_request(&e.0)))?,
                )
            }
        };
        // Everything is checked before any of it is written: `apply` writes only after every field passes, and the
        // secrets were sealed above.
        match id {
            CHAT_AGENT => program_settings::chat::apply(&folder, &env, &update.settings),
            _ => program_settings::ci::apply(&folder, &env, &update.settings),
        }
        .map_err(refused)?;
        if let Some(plan) = plan {
            self.secrets_for(program.owner, status.as_ref())
                .commit(plan)
                .map_err(|e| Box::new(internal_error(&format!("{e:#}"))))?;
        }
        Ok(())
    }
}

fn program_folder(id: &str, root: &Path) -> std::path::PathBuf {
    match id {
        CHAT_AGENT => paths::chat_dir(root),
        CI_RUNNER => paths::ci_dir(root),
        _ => paths::code_dir(root),
    }
}

/// `POST /api/settings/programs/{program}`'s body: a program's own settings (as `GET` shows them under `file`) and
/// its secrets (`{ NAME: value | null }`), either optional.
#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ProgramUpdate {
    pub(crate) settings: Map<String, Value>,
    pub(crate) secrets: Option<Value>,
}

pub(super) async fn get_programs(
    State(settings): State<Arc<PageHost>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if settings.authenticate(&headers).is_none() {
        return unauthorized("Sign in first.");
    }
    Json(settings.programs_view(settings.is_this_computer(peer.ip(), &headers))).into_response()
}

pub(super) async fn post_program(
    State(settings): State<Arc<PageHost>>,
    UrlPath(program): UrlPath<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let Some(session) = settings.authenticate(&headers) else {
        return unauthorized("Sign in first.");
    };
    if !csrf_ok(&headers, &session.csrf) {
        return unauthorized("Missing or wrong CSRF token.");
    }
    let Ok(update) = serde_json::from_slice::<ProgramUpdate>(&body) else {
        return bad_request("No JSON body, or a field this program doesn't have.");
    };
    match settings.save_program(&program, &update, settings.is_this_computer(peer.ip(), &headers)) {
        Ok(()) => Json(json!({ "saved": true })).into_response(),
        Err(response) => *response,
    }
}
