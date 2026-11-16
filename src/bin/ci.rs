//! `retaskable-ci`: CI for GitHub repos, a separate process from the Coding Agent (docs/specs/agent-ci.md). `link`
//! links it to the shared backlog as `role: "ci"`; `run` keeps checking the repos in `AGENT_CI_REPO`; `once <sha>...`
//! checks just the pull requests (or main) at those commits and exits.
use std::sync::Arc;

use anyhow::{Result, bail};
use retaskable_agent::agent_ci::store::StoreOps;
use retaskable_agent::agent_ci::{self, CiLoop, Settings, link as ci_link};
use retaskable_agent::env::Env;
use retaskable_agent::program_status::{self, StatusWriter};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const USAGE: &str = "retaskable-ci: CI for GitHub repos, run as its own process.

Usage:
  retaskable-ci link            Link this computer's CI runner to the shared backlog (role ci).
  retaskable-ci run             Check the repos in AGENT_CI_REPO or settings.json, again and again.
  retaskable-ci once <sha>...   Check just the pull requests or main at these commits, then exit.
  retaskable-ci --version

Settings: <data folder>/ci/settings.json (repos, slots, cpus, name), read again on each poll; the
environment below wins over it, field by field.

Environment:
  AGENT_CI_REPO         owner/repo, comma-separated for several (owner/repo@branch for a base other than main)
  AGENT_CI_SLOTS        checks at once (default 2; 0 runs nothing)
  AGENT_CI_CPUS         --cpus for each check (default 8, capped at what Docker has)
  AGENT_CI_NAME         the name in each status (default: this computer's hostname)
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
        Some("run") => {
            let (ci, cfg) = start(&env)?;
            let (reload_env, dir) = (env.clone(), cfg.data_dir.clone());
            let reload = move || Settings::load_in(&reload_env, dir.clone());
            tokio::select! {
                _ = agent_ci::run(ci, cfg, reload) => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Some("once") => {
            if args.len() < 2 {
                bail!("`once` needs at least one commit sha: retaskable-ci once <sha>...");
            }
            let (ci, cfg) = start(&env)?;
            let started = agent_ci::once(ci, cfg, &args[1..]).await;
            println!("Checked {started} commit(s).");
        }
        Some(other) => bail!("Unknown command {other:?}.\n\n{USAGE}"),
    }
    Ok(())
}

/// The loop and its settings, with the store's merge lock behind `automerge` when this computer is linked.
fn start(env: &Env) -> Result<(CiLoop, Settings)> {
    let cfg = Settings::load(env)?;
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
            program_status::env_set(env, &agent_ci::settings::ENV_VARS),
        );
    let names: Vec<_> = cfg.repos.iter().map(|r| r.github_repo.as_str()).collect();
    tracing::info!(
        "CI runner {}: {} slot(s), repos {}, data {}",
        cfg.computer,
        cfg.slots,
        names.join(", "),
        cfg.data_dir.display()
    );
    Ok((ci, cfg))
}
