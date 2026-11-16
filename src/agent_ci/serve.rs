//! The CI runner's own settings page (docs/specs/agent-ci.md section 3.9), for a computer that has no code agent to
//! host the page. The sign-in, sessions, Host/Origin checks and the programs panel's routes are `page_host`'s, the same
//! code the code agent's page runs; what is here is where network access is kept (`ci/settings.json`'s `network` key,
//! `program_settings::ci`), the trimmed page, `GET /api/health`, the port and the address it binds. It serves
//! settings and status and carries no command, like every settings page (programs.md).
use std::net::{Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use axum::response::Response;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;

use super::settings::data_root;
use crate::env::Env;
use crate::network::NetworkAccess;
use crate::page_host::{self, Access, PageHost, not_found_response};
use crate::program_settings::ci;

static PAGE: &str = include_str!("../../assets/settings-ci.html");

/// The port when neither `--port` nor `AGENT_CI_SETTINGS_PORT` says one: next to the code agent's 4700 and a Staging
/// build's 4701, so all three can share a computer.
pub const DEFAULT_PORT: u16 = 4702;
/// The environment variable that sets the port.
pub const PORT_VAR: &str = "AGENT_CI_SETTINGS_PORT";

/// The port: `flag` (`--port`), else `AGENT_CI_SETTINGS_PORT`, else [`DEFAULT_PORT`]; a whole number from 1024 to 65535,
/// the code agent's own range. One that isn't is an error rather than a quiet fallback to another port.
pub fn port(env: &Env, flag: Option<&str>) -> Result<u16> {
    let (text, from) = match (flag, env.get(PORT_VAR)) {
        (Some(flag), _) => (flag.to_string(), "--port"),
        (None, Some(v)) if !v.trim().is_empty() => (v, PORT_VAR),
        _ => return Ok(DEFAULT_PORT),
    };
    match text.trim().parse::<u16>() {
        Ok(port) if port >= 1024 => Ok(port),
        _ => bail!("{from} must be a whole number from 1024 to 65535."),
    }
}

/// Network access as `ci/settings.json` has it, read on each request so `retaskable-ci network off` takes effect at
/// once. Unreadable is `None`: refused like off (fail closed).
struct CiAccess {
    dir: std::path::PathBuf,
}

impl Access for CiAccess {
    fn network_access(&self) -> Option<NetworkAccess> {
        ci::network(&self.dir).ok()
    }

    fn password_signs_in_locally(&self) -> bool {
        true
    }
}

/// The page's state over the data folder `root`, answering on `port`. The runner's folder is made if it isn't there, so
/// the page can set the runner up before it has ever run.
pub fn host(env: &Env, root: &Path, port: u16, now: impl Fn() -> u64 + Send + Sync + 'static) -> Result<Arc<PageHost>> {
    let dir = crate::paths::ci_dir(root);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(Arc::new(PageHost::new(
        root.to_path_buf(),
        port,
        env.clone(),
        now,
        CiAccess { dir },
    )))
}

/// Every route the CI runner's port has: `GET /api/health`, and `page_host`'s (the page, sign-in, the programs panel).
/// Any other path is a 404 (`routes_tests.rs` lists them).
pub fn router(host: Arc<PageHost>, version: &str) -> Router {
    let version = version.to_string();
    Router::new()
        .route(
            "/api/health",
            get(move || {
                let version = version.clone();
                async move { Json(json!({ "ok": true, "version": version })) }
            })
            .fallback(not_found),
        )
        .merge(page_host::routes(host, page_host::assemble(PAGE)))
        .fallback(not_found)
}

async fn not_found() -> Response {
    not_found_response()
}

/// Where to listen: this computer only, unless network access is on with a password (agent-settings.md section 4.4).
pub fn bind_addr(network_on: bool, port: u16) -> SocketAddr {
    let host = if network_on {
        Ipv4Addr::UNSPECIFIED
    } else {
        Ipv4Addr::LOCALHOST
    };
    SocketAddr::from((host, port))
}

/// Serves the page until the process stops. Says (through `say`) where it is, with the one-time sign-in link, good once
/// for five minutes and taken only from this computer.
pub async fn serve(env: &Env, port: u16, version: &str, say: impl Fn(&str)) -> Result<()> {
    let root = data_root(env);
    let host = host(env, &root, port, || crate::program_status::now_ms() / 1000)?;
    // The data key of a saved token lives in the OS credential store, as for the code agent's page.
    host.set_program_keys(crate::link::os_store(crate::secrets::KEY_SERVICE));
    let network_on = ci::network(&crate::paths::ci_dir(&root)).is_ok_and(|a| a.enabled);
    let addr = bind_addr(network_on, port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("listening on {addr}"))?;
    say(&format!(
        "CI runner settings page, open it once with: {}",
        host.settings_link()
    ));
    say(
        "That link works once, for 5 minutes, from this computer. After that sign in with `retaskable-ci set-password`'s password.",
    );
    if network_on {
        say(&format!(
            "Network access is on: the page is also open on the network, at http://{addr}/settings"
        ));
    }
    axum::serve(
        listener,
        router(host, version).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("serving the settings page")
}

/// `retaskable-ci set-password`: saves the sign-in password (kept only as its scrypt hash) in the runner's own settings
/// file. A running page reads it on the next sign-in; whether other computers may reach the page is `set_network`'s.
pub fn set_password(env: &Env, password: &str) -> Result<()> {
    let dir = runner_dir(env)?;
    ci::set_password(&dir, password).map_err(|r| anyhow::anyhow!(r.0))
}

/// `retaskable-ci network on|off`: whether the page is reachable from another computer (it listens on every address
/// instead of this computer's only, from the next start). On needs a password set first.
pub fn set_network(env: &Env, on: bool) -> Result<()> {
    let dir = runner_dir(env)?;
    ci::set_network(&dir, on).map_err(|r| anyhow::anyhow!(r.0))
}

fn runner_dir(env: &Env) -> Result<std::path::PathBuf> {
    let dir = crate::paths::ci_dir(&data_root(env));
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// A password piped in on stdin: its first line, without the line ending, and nothing else trimmed (a space is part of
/// a password).
pub fn password_from_line(line: &str) -> String {
    line.trim_end_matches(['\n', '\r']).to_string()
}

#[cfg(test)]
mod tests;
