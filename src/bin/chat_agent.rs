//! `retaskable-chat-agent`: the Chat Agent, a separate agent from the Coding Agent that takes one person's chat messages
//! (chat-agent-process.md). `link` links it to retaskable.com and the shared backlog as `role: "chat"`; `run` (the
//! default) keeps a socket open for each project and answers what the hub sends.
use std::sync::Arc;

use anyhow::{Result, bail};
use retaskable_agent::backlog::Client;
use retaskable_agent::chat::answer::Project;
use retaskable_agent::chat::board::StoreBoard;
use retaskable_agent::chat::claude::{ClaudeRunner, Runner, scratch_dir};
use retaskable_agent::chat::hub::{Chat, run_project};
use retaskable_agent::chat::{link as chat_link, project};
use retaskable_agent::config;
use retaskable_agent::connector::Timing;
use retaskable_agent::env::Env;
use retaskable_agent::link::{BACKLOG_TARGET, CLOUD_TARGET};
use retaskable_agent::program_status::{self, Linked, Report, StatusWriter};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("RETASKABLE_LOG").unwrap_or_else(|_| "info".into()),
        )
        .init();
    let env = Env::from_process();
    match std::env::args().nth(1).as_deref() {
        Some("--version") => println!("{VERSION}"),
        Some("link") => chat_link::run(&env, VERSION, |line| println!("{line}")).await?,
        Some("run") | None => run(&env).await?,
        Some(other) => {
            bail!("Unknown command {other:?}. Use `retaskable-chat-agent link` or `retaskable-chat-agent run`.")
        }
    }
    Ok(())
}

async fn run(env: &Env) -> Result<()> {
    let cfg = config::load(&chat_link::root(env))?;
    let (cloud_url, store_url) = chat_link::urls(env)?;
    let http = reqwest::Client::new();
    let cloud = Arc::new(chat_link::tokens(env, cloud_url.clone(), CLOUD_TARGET, http.clone()));
    let runner_probe = ClaudeRunner::new(env);
    if !runner_probe.available() {
        // Said once, here; each ask is also nacked with the reason, so nothing is filed or guessed without it.
        tracing::warn!("There is no Claude Code on this computer: the Chat Agent will refuse every message.");
    }
    let host = retaskable_agent::link::machine_name(env);
    let log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(|line| tracing::info!("{line}"));
    let mut hubs = tokio::task::JoinSet::new();
    for p in &cfg.projects {
        let id = p.id.clone().unwrap_or_default();
        let repo_id = match project::repo_id(env, &p.github_repo).await {
            Ok(id) => id,
            Err(why) => {
                tracing::warn!("Project {id:?} skipped: {why}");
                continue;
            }
        };
        let board = StoreBoard {
            client: Client::new(store_url.clone()),
            tokens: chat_link::tokens(env, store_url.clone(), BACKLOG_TARGET, http.clone()),
            repo_id,
        };
        let state = Project {
            github_repo: p.github_repo.clone(),
            base_branch: p.base_branch.clone(),
        };
        let chat = Arc::new(Chat::new(
            board,
            ClaudeRunner::new(env),
            state,
            scratch_dir(),
            host.clone(),
        ));
        hubs.spawn(run_project(
            chat,
            cloud.clone(),
            cloud_url.clone(),
            id,
            project::hello(env, VERSION, p),
            Timing::default(),
            log.clone(),
        ));
    }
    if hubs.is_empty() {
        bail!("No project to serve. Add a project with a GitHub repository to the settings, then run again.");
    }
    tracing::info!("{}", backlog_note(&store_url));
    // programs-status.md: a status file in the chat folder, written every few seconds while this runs.
    let (started, served, claude) = (program_status::now_ms(), hubs.len(), runner_probe.available());
    let (keys, env_set) = (chat_link::token_store(env), program_status::env_set(env, &ENV_VARS));
    let status = StatusWriter::new(&chat_link::data_dir(env), program_status::CHAT_AGENT, VERSION, started);
    let linked = program_status::LinkedCache::new(program_status::LINKED_TTL, move || {
        let held = |url: &str| matches!(keys.get(url), Ok(Some(_)));
        Linked {
            store: held(&store_url),
            cloud: Some(held(&cloud_url)),
        }
    });
    let _status = status.spawn(move || {
        let mut extra = serde_json::Map::new();
        extra.insert("claudeAvailable".into(), claude.into());
        extra.insert("projectsServed".into(), served.into());
        Report {
            linked: linked.get(program_status::now_ms()),
            env_set: env_set.clone(),
            extra,
            ..Report::default()
        }
    });
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = hubs.join_next() => {}
    }
    cloud.settled(std::time::Duration::from_secs(5), || {}).await;
    Ok(())
}

/// The environment variables that set the Chat Agent (the status file lists which are set, never their values).
const ENV_VARS: [&str; 3] = [
    "RETASKABLE_DATA_DIR",
    "RETASKABLE_BACKLOG_URL",
    "RETASKABLE_CHAT_CREDENTIALS_FILE",
];

fn backlog_note(url: &str) -> String {
    format!(
        "Chat Agent serving; shared backlog {}",
        url.trim_start_matches("https://").trim_start_matches("http://")
    )
}
