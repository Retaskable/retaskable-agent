//! `retaskable-ci`: CI for GitHub repos, a separate process from the Coding Agent (docs/specs/agent-ci.md). `link`
//! links it to the shared backlog as `role: "ci"`; `run` keeps checking the repos in `AGENT_CI_REPO`; `once <sha>...`
//! checks just the pull requests (or main) at those commits and exits. `serve-settings`, `set-password` and `network`
//! are for a computer with no code agent, whose settings page this then hosts (section 3.9).
use std::io::{BufRead, IsTerminal};
use std::sync::Arc;

use anyhow::{Result, bail};
use retaskable_agent::agent_ci::serve;
use retaskable_agent::agent_ci::store::StoreOps;
use retaskable_agent::agent_ci::{self, CiLoop, Settings, link as ci_link};
use retaskable_agent::env::Env;
use retaskable_agent::program_status::{self, StatusWriter};
use retaskable_agent::secrets::{Owner, Secrets};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "retaskable-ci: CI for GitHub repos, run as its own process.

Usage:
  retaskable-ci link            Link this computer's CI runner to the shared backlog (role ci).
  retaskable-ci run [--settings-port N]
                                Check the repos in AGENT_CI_REPO or settings.json, again and again. With
                                --settings-port (or AGENT_CI_SETTINGS_PORT) it also hosts the settings page.
  retaskable-ci serve-settings [--port N]
                                Host the settings page alone (default port 4702) and print its one-time sign-in link.
  retaskable-ci set-password    Set the password the page signs in with (asked twice, never echoed; or piped on stdin).
  retaskable-ci secret set <NAME>
                                Save a secret (GH_TOKEN), sealed under the data key: asked for without echo, or piped on
                                stdin; never an argument. `secret list` shows each name, its source and fingerprint.
  retaskable-ci network on|off  Let other computers reach the page (needs a password; from the next start). Off by default.
  retaskable-ci once <sha>...   Check just the pull requests or main at these commits, then exit.
  retaskable-ci --version

Settings: <data folder>/ci/settings.json (repos, slots, cpus, name), read again on each poll; the
environment below wins over it, field by field.

Environment:
  AGENT_CI_REPO         owner/repo, comma-separated for several (owner/repo@branch for a base other than main)
  AGENT_CI_SLOTS        checks at once (default 2; 0 runs nothing)
  AGENT_CI_CPUS         --cpus for each check (default 8, capped at what Docker has)
  AGENT_CI_NAME         the name in each status (default: this computer's hostname)
  AGENT_CI_SETTINGS_PORT  the settings page's port, 1024 to 65535 (default 4702); it listens on 127.0.0.1 only
                        unless `network on` (or RETASKABLE_TRUST_DOCKER_HOST=1 in a container: 0.0.0.0 inside it,
                        and the container's gateway counts as this computer; publish the port on 127.0.0.1 only)
  RETASKABLE_DATA_DIR   the data folder (the CI runner keeps its files in its ci subfolder)";

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RETASKABLE_LOG").unwrap_or_else(|_| "info".into()),
        )
        .init();
    let env = Env::from_process();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version") => println!("{VERSION}"),
        Some("--help" | "-h" | "help") | None => println!("{USAGE}"),
        Some("link") => ci_link::run(&env, VERSION, |line| println!("{line}")).await?,
        Some("serve-settings") => {
            let port = serve::port(&env, flag(&args[1..], "--port")?.as_deref())?;
            tokio::select! {
                served = serve::serve(&env, port, VERSION, |line| println!("{line}")) => served?,
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Some("set-password") => {
            serve::set_password(&env, &new_password()?)?;
            println!(
                "Saved. It signs in on this computer; `retaskable-ci network on` lets other computers use it too."
            );
        }
        Some("secret") => {
            let root = agent_ci::settings::data_root(&env);
            println!(
                "{}",
                retaskable_agent::secrets::cli::run(Owner::CiRunner, &root, &env, &args[1..])?
            );
        }
        Some("network") => {
            let on = match args.get(1).map(String::as_str) {
                Some("on") => true,
                Some("off") => false,
                _ => bail!("Usage: retaskable-ci network on|off"),
            };
            serve::set_network(&env, on)?;
            println!(
                "Network access is {}. It takes effect the next time the settings page starts.",
                if on { "on" } else { "off" }
            );
        }
        Some("run") => {
            // The settings page beside the checks, so the panel's CI status is the running loop's own.
            let flagged = flag(&args[1..], "--settings-port")?;
            if flagged.is_some() || env.get(serve::PORT_VAR).is_some() {
                let (page_env, port) = (env.clone(), serve::port(&env, flagged.as_deref())?);
                tokio::spawn(async move {
                    if let Err(e) = serve::serve(&page_env, port, VERSION, |line| println!("{line}")).await {
                        tracing::error!("The settings page isn't up: {e:#}");
                    }
                });
            }
            let (ci, cfg, secrets) = start(&env)?;
            let (reload_env, dir) = (env.clone(), cfg.data_dir.clone());
            // A token saved on the settings page (another process) reaches the checks started after the next poll.
            let reload = move || {
                secrets.refresh();
                Settings::load_in(&reload_env, dir.clone())
            };
            tokio::select! {
                _ = agent_ci::run(ci, cfg, reload) => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Some("once") => {
            if args.len() < 2 {
                bail!("`once` needs at least one commit sha: retaskable-ci once <sha>...");
            }
            let (ci, cfg, _) = start(&env)?;
            let started = agent_ci::once(ci, cfg, &args[1..]).await;
            println!("Checked {started} commit(s).");
        }
        Some(other) => bail!("Unknown command {other:?}.\n\n{USAGE}"),
    }
    Ok(())
}

/// The value after `name` in `args` (`--port 4702`), or none when the flag is absent.
fn flag(args: &[String], name: &str) -> Result<Option<String>> {
    match args.iter().position(|a| a == name) {
        None => Ok(None),
        Some(at) => match args.get(at + 1) {
            Some(value) => Ok(Some(value.clone())),
            None => bail!("{name} needs a value."),
        },
    }
}

/// The new password: asked twice without echo at a terminal, else the first line of stdin. Never from an argument,
/// which would sit in the process list and the shell's history.
fn new_password() -> Result<String> {
    if std::io::stdin().is_terminal() {
        let first = rpassword::prompt_password("New password (at least 8 characters): ")?;
        if rpassword::prompt_password("Again: ")? != first {
            bail!("The two passwords differ; nothing was saved.");
        }
        return Ok(first);
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(serve::password_from_line(&line))
}

/// The loop and its settings, with the store's merge lock behind `automerge` when this computer is linked.
fn start(env: &Env) -> Result<(CiLoop, Settings, Arc<Secrets>)> {
    let cfg = Settings::load(env)?;
    // The saved GH_TOKEN (encrypted in the settings file) goes into this process's environment, before any check starts.
    let secrets = Secrets::for_program(Owner::CiRunner, &agent_ci::settings::data_root(env), env);
    secrets.apply();
    let url = ci_link::store_url(env)?;
    let keychain = ci_link::token_store(env);
    if !matches!(keychain.get(&url), Ok(Some(_))) {
        tracing::warn!(
            "The CI runner isn't linked to the shared backlog (run `retaskable-ci link`): it merges nothing."
        );
    }
    let linked = {
        let (keychain, url) = (keychain.clone(), url.clone());
        let cache = program_status::LinkedCache::new(program_status::LINKED_TTL, move || program_status::Linked {
            store: matches!(keychain.get(&url), Ok(Some(_))),
            cloud: None,
        });
        Arc::new(move || cache.get(program_status::now_ms()).store)
    };
    let status = StatusWriter::new(
        &cfg.data_dir,
        program_status::CI_RUNNER,
        VERSION,
        program_status::now_ms(),
    );
    let ci = CiLoop::new(Default::default())
        .with_store(Arc::new(StoreOps::new(url, keychain)))
        .with_status(
            status,
            linked,
            [
                program_status::env_set(env, &agent_ci::settings::ENV_VARS),
                secrets.env_names(),
            ]
            .concat(),
        );
    let names: Vec<_> = cfg.repos.iter().map(|r| r.github_repo.as_str()).collect();
    tracing::info!(
        "CI runner {}: {} slot(s), repos {}, data {}",
        cfg.computer,
        cfg.slots,
        names.join(", "),
        cfg.data_dir.display()
    );
    Ok((ci, cfg, secrets))
}
