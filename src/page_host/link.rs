//! The **Link** button of the Chat agent's and CI runner's tabs (agent-settings.md section 4.6): `POST
//! /api/settings/programs/{program}/link` runs that program's device flow (`program_link`, the same one its own `link`
//! command runs, its role and its own credential identity) in this process, and the programs' rows in `GET
//! /api/settings/programs` carry what it is waiting on. The page can't call a program (there is no channel), but both
//! share the data folder, so the token goes where the program's own command keeps it, and the running program finds it
//! on its next refresh: nothing is restarted.
//!
//! Like a saved setting this changes only what the person asked, and only from this computer: a session, the CSRF
//! token, the Host and Origin checks, and `is_this_computer`. One flow at a time per program. What is sent to the page
//! is the address that approves it and the short code a person reads off it, never a token or the device code.
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Json;
use axum::extract::{ConnectInfo, Path as UrlPath, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use tokio::sync::watch;

use super::auth::csrf_ok;
use super::{PageHost, bad_request, json_error, not_found_response, unauthorized};
use crate::link::{DeviceCode, Link};
use crate::program_link::{self, Spec};
use crate::program_status::{self, ProgramStatus, State as Run};

/// A device code's own lifetime is ten minutes; this is the most a flow is let wait for the store to say so.
const FLOW_LIMIT: Duration = Duration::from_secs(15 * 60);

/// What a person approves: the store's page with the code filled in, and the code to type if it isn't.
#[derive(Clone)]
struct Approve {
    url: Option<String>,
    code: String,
}

#[derive(Clone)]
enum Flow {
    /// Step `no` of `of`, with the code once the store has given one.
    Working {
        no: usize,
        of: usize,
        label: &'static str,
        approve: Option<Approve>,
    },
    Failed(String),
    Done,
}

/// The links this page has started, one per program, and what wakes a watcher on every change.
pub(crate) struct Flows {
    state: Mutex<HashMap<&'static str, Flow>>,
    second: Mutex<Duration>,
    changes: watch::Sender<u64>,
}

impl Default for Flows {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            second: Mutex::new(Duration::from_secs(1)),
            changes: watch::channel(0).0,
        }
    }
}

impl Flows {
    /// What one second of the store's polling interval is: a second, or less in a test.
    #[cfg(test)]
    pub(crate) fn set_second(&self, second: Duration) {
        *self.second.lock().unwrap() = second;
    }

    /// A receiver that changes whenever a flow's state does.
    #[cfg(test)]
    pub(crate) fn subscribe(&self) -> watch::Receiver<u64> {
        self.changes.subscribe()
    }

    fn set(&self, program: &'static str, flow: Flow) {
        self.state.lock().unwrap().insert(program, flow);
        self.changes.send_modify(|n| *n += 1);
    }

    fn get(&self, program: &str) -> Option<Flow> {
        self.state.lock().unwrap().get(program).cloned()
    }

    /// Takes the program's place for a new flow, unless one is under way.
    fn begin(&self, program: &'static str) -> bool {
        let mut state = self.state.lock().unwrap();
        if matches!(state.get(program), Some(Flow::Working { .. })) {
            return false;
        }
        state.insert(
            program,
            Flow::Working {
                no: 0,
                of: 0,
                label: "",
                approve: None,
            },
        );
        true
    }
}

impl PageHost {
    /// The running page's version, sent with a link's code request (a release build's, not this crate's).
    pub fn set_version(&self, version: &str) {
        let _ = self.version.set(version.to_string());
    }

    fn version(&self) -> &str {
        self.version
            .get()
            .map(String::as_str)
            .unwrap_or(env!("CARGO_PKG_VERSION"))
    }

    /// Why the page can't link `spec` itself, for the person to read; `None` when it can. A program that has an address
    /// or a token file of its own set in its environment keeps them there, where this page can't see them.
    fn cannot_link(&self, spec: &Spec, status: Option<&ProgramStatus>) -> Option<String> {
        let own = |name: &str| status.is_some_and(|s| s.env_set.iter().any(|n| n == name));
        let mut vars = vec![spec.file_var, "RETASKABLE_BACKLOG_URL"];
        if spec.cloud {
            vars.push("RETASKABLE_CLOUD_URL");
        }
        if let Some(var) = vars.into_iter().find(|v| own(v) && self.env.get(v).is_none()) {
            return Some(format!(
                "{} sets {var} in its own environment, which this page can't see: link it from a terminal on its computer.",
                spec.label
            ));
        }
        spec.steps(&self.env).err().map(|e| format!("{e:#}"))
    }

    /// What `GET /api/settings/programs` says about linking `spec`.
    pub(super) fn link_view(&self, spec: &Spec, status: Option<&ProgramStatus>, here: bool) -> Value {
        let steps = spec.steps(&self.env).unwrap_or_default();
        let store = spec.store(&self.dir, &self.env);
        // Linked when the store this page can see holds every token, or the program itself says it holds them (it reads
        // its own store, which this page may not be able to see).
        let held = !steps.is_empty() && steps.iter().all(|s| matches!(store.get(&s.url), Ok(Some(_))));
        let says = status.is_some_and(|s| s.linked.store && s.linked.cloud.unwrap_or(true));
        let linked = held || says;
        let flow = self.program_links.get(spec.program);
        let mut view = json!({
            "linked": linked,
            // Whether this request is from this computer: the only place a link is started from.
            "here": here,
            "why": self.cannot_link(spec, status),
            "working": false,
            "step": null,
            "approve": null,
            "error": null,
        });
        match flow {
            Some(Flow::Working { no, of, label, approve }) => {
                view["working"] = json!(true);
                view["step"] = json!({ "no": no, "of": of, "label": label });
                if let Some(a) = approve {
                    view["approve"] = json!({ "url": a.url, "code": a.code });
                }
            }
            Some(Flow::Failed(why)) if !linked => view["error"] = json!(why),
            _ => {}
        }
        view
    }

    /// Starts the program's device flow in a task of this process.
    fn start_link(self: &Arc<Self>, spec: &'static Spec) -> Result<(), Box<Response>> {
        let folder = spec.folder(&self.dir);
        let now = (self.now)().saturating_mul(1000);
        if program_status::state_in(&folder, now) == Run::NotInstalled {
            return Err(Box::new(bad_request("That program isn't installed on this computer.")));
        }
        let status = program_status::read(&folder);
        if let Some(why) = self.cannot_link(spec, status.as_ref()) {
            return Err(Box::new(bad_request(&why)));
        }
        let steps = spec
            .steps(&self.env)
            .map_err(|e| Box::new(bad_request(&format!("{e:#}"))))?;
        let store = spec.store(&self.dir, &self.env);
        let todo: Vec<_> = steps
            .into_iter()
            .filter(|s| !matches!(store.get(&s.url), Ok(Some(_))))
            .collect();
        if todo.is_empty() {
            return Err(Box::new(json_error(StatusCode::CONFLICT, "Already linked.")));
        }
        if !self.program_links.begin(spec.program) {
            return Err(Box::new(json_error(
                StatusCode::CONFLICT,
                "A link is already waiting for approval.",
            )));
        }
        let host = self.clone();
        tokio::spawn(async move {
            let of = todo.len();
            let machine = spec.machine(&host.env, host.version());
            for (i, step) in todo.iter().enumerate() {
                let second = *host.program_links.second.lock().unwrap();
                let working = |approve| Flow::Working {
                    no: i + 1,
                    of,
                    label: step.label,
                    approve,
                };
                host.program_links.set(spec.program, working(None));
                let flow = Link {
                    http: reqwest::Client::new(),
                    store: store.as_ref(),
                    machine: machine.clone(),
                    target: step.target,
                    second,
                };
                let shown = |c: &DeviceCode| {
                    let url = crate::link::pending::web_address(&c.verification_uri_complete);
                    let approve = Approve {
                        url,
                        code: c.user_code.clone(),
                    };
                    host.program_links.set(spec.program, working(Some(approve)));
                };
                let end = match tokio::time::timeout(FLOW_LIMIT, flow.run(&step.url, shown)).await {
                    Ok(Ok(_)) => continue,
                    Ok(Err(e)) => format!("{e:#}"),
                    Err(_) => "The code expired before it was approved. Link again.".to_string(),
                };
                tracing::warn!("Couldn't link the {}: {end}", spec.label);
                host.program_links.set(spec.program, Flow::Failed(end));
                return;
            }
            tracing::info!("Linked the {} from the settings page.", spec.label);
            host.program_links.set(spec.program, Flow::Done);
        });
        Ok(())
    }
}

/// `POST /api/settings/programs/{program}/link`: no body. Refused from another computer, without a session, a CSRF
/// token or the right Origin and Host, for a program the page can't link, one that is linked, or one that is waiting.
pub(super) async fn post_program_link(
    State(host): State<Arc<PageHost>>,
    UrlPath(program): UrlPath<String>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let Some(session) = host.authenticate(&headers) else {
        return unauthorized("Sign in first.");
    };
    if !csrf_ok(&headers, &session.csrf) {
        return unauthorized("Missing or wrong CSRF token.");
    }
    if !host.is_this_computer(peer.ip(), &headers) {
        return json_error(StatusCode::FORBIDDEN, "Linking is done only on this computer.");
    }
    let Some(spec) = program_link::spec(&program) else {
        return not_found_response();
    };
    match host.start_link(spec) {
        Ok(()) => Json(json!({ "linking": true })).into_response(),
        Err(refused) => *refused,
    }
}
