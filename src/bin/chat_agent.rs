//! `retaskable-chat-agent`: the Chat Agent, a separate agent from the Coding Agent that takes one person's chat messages
//! (chat-agent-process.md). `link` links it to retaskable.com and the shared backlog as `role: "chat"`; `run` (the
//! default) asks the store which projects it serves (`projects_served`), keeps a socket open for each and answers
//! what the hub sends.
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use retaskable_agent::backlog::Client;
use retaskable_agent::chat::answer::Project;
use retaskable_agent::chat::board::StoreBoard;
use retaskable_agent::chat::claude::{ClaudeRunner, Runner, scratch_dir};
use retaskable_agent::chat::hub::{Chat, run_project};
use retaskable_agent::chat::{link as chat_link, project, served};
use retaskable_agent::connector::Timing;
use retaskable_agent::env::Env;
use retaskable_agent::link::{BACKLOG_TARGET, CLOUD_TARGET};
use retaskable_agent::program_settings::chat as chat_settings;
use retaskable_agent::program_status::{self, Linked, Report, StatusWriter};
use retaskable_agent::secrets::{Owner, Secrets};

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
    let (cloud_url, store_url) = chat_link::urls(env)?;
    let http = reqwest::Client::new();
    let cloud = Arc::new(chat_link::tokens(env, cloud_url.clone(), CLOUD_TARGET, http.clone()));
    let store_tokens = chat_link::tokens(env, store_url.clone(), BACKLOG_TARGET, http.clone());
    let store = Client::new(store_url.clone());
    let runner_probe = ClaudeRunner::new(env);
    if !runner_probe.available() {
        // Said once, here; each ask is also nacked with the reason, so nothing is filed or guessed without it.
        tracing::warn!("There is no Claude Code on this computer: the Chat Agent will refuse every message.");
    }
    let host = retaskable_agent::link::machine_name(env);
    let log: Arc<dyn Fn(String) + Send + Sync> = Arc::new(|line| tracing::info!("{line}"));
    tracing::info!("{}", backlog_note(&store_url));
    // programs-status.md: a status file in the chat folder, written every few seconds while this runs.
    let (started, claude) = (program_status::now_ms(), runner_probe.available());
    let projects_running = Arc::new(AtomicUsize::new(0));
    let last_error: Arc<Mutex<Option<String>>> = Arc::default();
    // The saved Claude Code token or API key (encrypted in chat/settings.json) goes in the environment its sessions inherit.
    let secrets = Secrets::for_program(Owner::ChatAgent, &chat_link::root(env), env);
    secrets.apply();
    let env_set = [program_status::env_set(env, &ENV_VARS), secrets.env_names()].concat();
    let keys = chat_link::token_store(env);
    let status = StatusWriter::new(&chat_link::data_dir(env), program_status::CHAT_AGENT, VERSION, started);
    let linked = {
        let (store_url, cloud_url) = (store_url.clone(), cloud_url.clone());
        program_status::LinkedCache::new(program_status::LINKED_TTL, move || {
            let held = |url: &str| matches!(keys.get(url), Ok(Some(_)));
            Linked {
                store: held(&store_url),
                cloud: Some(held(&cloud_url)),
            }
        })
    };
    let _status = {
        let (running_now, error_now) = (projects_running.clone(), last_error.clone());
        status.spawn(move || {
            let mut extra = serde_json::Map::new();
            extra.insert("claudeAvailable".into(), claude.into());
            extra.insert("projectsServed".into(), running_now.load(Ordering::Relaxed).into());
            Report {
                linked: linked.get(program_status::now_ms()),
                last_error: error_now.lock().ok().and_then(|e| e.clone()),
                env_set: env_set.clone(),
                extra,
            }
        })
    };
    // The projects come from the store (chat-agent-process.md section 5.1), asked now and every few minutes: one
    // socket per project, started when it appears and stopped when the store stops naming it.
    let mut hubs = tokio::task::JoinSet::new();
    let mut running: HashMap<u64, (served::Wanted, tokio::task::AbortHandle)> = HashMap::new();
    let mut list: Vec<served::Wanted> = Vec::new();
    let mut tick = tokio::time::interval(served::REFRESH);
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            Some(ended) = hubs.join_next() => {
                // A socket task only ends if it panicked; it is started again at the next refresh.
                if let Err(e) = ended
                    && !e.is_cancelled()
                {
                    tracing::warn!("A project's socket stopped: {e}");
                }
                running.retain(|_, (_, h)| !h.is_finished());
            }
            _ = tick.tick() => {
                secrets.refresh();
                // The settings file is read again each refresh, so a change on the settings page applies without a restart.
                let (only, settings_error) = chat_settings::only_repo(env, &chat_link::data_dir(env));
                let outcome = served::fetch(&store, &store_tokens, only.as_deref()).await;
                let (next, error) = served::next(&list, outcome);
                let error = error.or(settings_error);
                match &error {
                    Some(why) => tracing::warn!("Chat Agent couldn't learn its projects: {why}"),
                    None if next.is_empty() => tracing::info!(
                        "The store names no project for this Chat Agent yet: one of your agents has to connect to it first."
                    ),
                    None => {}
                }
                running.retain(|id, (_, handle)| {
                    let keep = next.iter().any(|w| w.repo_id == *id);
                    if !keep {
                        tracing::info!("Project {id} is no longer served.");
                        handle.abort();
                    }
                    keep
                });
                for w in &next {
                    // A new project, or one whose repository or branch the store now says differently.
                    if running.get(&w.repo_id).is_some_and(|(have, _)| have == w) {
                        continue;
                    }
                    if let Some((_, old)) = running.remove(&w.repo_id) {
                        old.abort();
                    }
                    let handle = start(env, w, &store_url, &http, &host, &cloud, &cloud_url, &log, &mut hubs);
                    running.insert(w.repo_id, (w.clone(), handle));
                }
                list = next;
                projects_running.store(running.len(), Ordering::Relaxed);
                if let Ok(mut e) = last_error.lock() {
                    *e = error;
                }
            }
        }
    }
    cloud.settled(std::time::Duration::from_secs(5), || {}).await;
    Ok(())
}

/// One project's chat and its socket to the hub, as a task of `hubs`.
#[allow(clippy::too_many_arguments)]
fn start(
    env: &Env,
    w: &served::Wanted,
    store_url: &str,
    http: &reqwest::Client,
    host: &str,
    cloud: &Arc<retaskable_agent::link::DeviceTokens>,
    cloud_url: &str,
    log: &Arc<dyn Fn(String) + Send + Sync>,
    hubs: &mut tokio::task::JoinSet<()>,
) -> tokio::task::AbortHandle {
    let board = StoreBoard {
        client: Client::new(store_url.to_string()),
        tokens: chat_link::tokens(env, store_url.to_string(), BACKLOG_TARGET, http.clone()),
        repo_id: w.repo_id,
    };
    let state = Project {
        github_repo: w.github_repo.clone(),
        base_branch: w.base_branch.clone(),
    };
    let chat = Arc::new(Chat::new(
        board,
        ClaudeRunner::new(env),
        state,
        scratch_dir(),
        host.to_string(),
    ));
    tracing::info!("Serving {} ({}).", w.github_repo, w.base_branch);
    hubs.spawn(run_project(
        chat,
        cloud.clone(),
        cloud_url.to_string(),
        project::socket_id(&w.github_repo),
        project::hello(env, VERSION, &w.github_repo),
        Timing::default(),
        log.clone(),
    ))
}

/// The environment variables that set the Chat Agent (the status file lists which are set, never their values).
const ENV_VARS: [&str; 5] = [
    "RETASKABLE_DATA_DIR",
    "RETASKABLE_BACKLOG_URL",
    "RETASKABLE_CLOUD_URL",
    "RETASKABLE_CHAT_REPO_ID",
    "RETASKABLE_CHAT_CREDENTIALS_FILE",
];

fn backlog_note(url: &str) -> String {
    format!(
        "Chat Agent serving; shared backlog {}",
        url.trim_start_matches("https://").trim_start_matches("http://")
    )
}
