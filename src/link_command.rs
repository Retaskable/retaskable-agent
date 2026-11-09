//! `retaskable link`: the device flow in the terminal, and the shared backlog's automatic link.
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use retaskable_agent::env::Env;
use retaskable_agent::{backlog, config, link, release, setup_questions};

/// `retaskable link [production|staging|local]`, or `retaskable link backlog`
/// (installers/04-every-agent-on-the-shared-backlog-shared.md, `npm run link -- backlog`): the device
/// flow in the terminal. The exit code.
pub async fn link_command(
    args: &[String],
    cfg: &config::ConfigFile,
    dir: &Path,
    env: &Env,
    version: &str,
    build: Option<&release::Build>,
) -> i32 {
    if args == [String::from("backlog")] {
        let keychain = link::keychain(dir, env);
        let url = backlog::url(env);
        return match run_link_in_terminal(&keychain, &url, link::BACKLOG_TARGET, env, version).await {
            Ok(id) => {
                println!(
                    "Linked to the shared backlog as agent {id}. The refresh token is in this machine's credential store."
                );
                0
            }
            Err(e) => {
                eprintln!("{e}");
                1
            }
        };
    }
    // Section 3.8 "One environment": a release build is fixed to `build.json`'s cloud, so linking
    // to any other is refused rather than silently ignored.
    let url = match args {
        [] => link::cloud_url(cfg, env, build).map_err(|e| format!("{e:#}")),
        [name] => match build {
            Some(b) if b.environment.name() != name => Err(format!(
                "This release build is fixed to {}, so it refuses to link to {name}.",
                b.environment.name()
            )),
            _ => link::environment_url(name).map(String::from).ok_or_else(String::new),
        },
        _ => Err(String::new()),
    };
    let url = match url {
        Ok(url) => url,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("{e}");
            }
            eprintln!("Usage: retaskable link [production|staging|local] | backlog");
            return 2;
        }
    };
    let keychain = link::keychain(dir, env);
    match run_link_in_terminal(&keychain, &url, link::CLOUD_TARGET, env, version).await {
        Ok(id) => {
            println!("Linked to {url} as agent {id}. The refresh token is in this machine's credential store.");
            // agent-settings.md section 5: the computer-only settings, asked only at a terminal.
            // Behind the `machine-settings` flag (Off: nothing is asked, as before). The flag client's
            // `PROJECT_ID` (src/flags.rs) and `RETASKABLE_FLAG_KEY` are placeholders the owner fills
            // in from Settings -> Project settings -> Release flags.
            let flag_on = retaskable_agent::flags::Flags::from_env().on("machine-settings").await;
            if let Err(e) = ask_setup_questions(dir, env, release::default_port(build), flag_on) {
                eprintln!("Couldn't ask or save the settings: {e:#}");
            }
            // installers/04-every-agent-on-the-shared-backlog-shared.md: the shared backlog's own
            // approval comes right after this one, opened by the agent itself.
            auto_link_backlog(
                link::pending::Links::new(keychain.clone(), reqwest::Client::new(), env, version),
                keychain.clone(),
                reqwest::Client::new(),
                env.clone(),
                version.to_string(),
            )
            .await;
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// Asks the computer-only settings on the real terminal (`setup_questions`).
fn ask_setup_questions(dir: &Path, env: &Env, default_port: u16, flag_on: bool) -> Result<()> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    let mut secret = setup_questions::read_secret;
    let is_terminal = stdin.is_terminal();
    let mut term = setup_questions::Terminal {
        is_terminal,
        input: &mut input,
        out: &mut out,
        secret: &mut secret,
    };
    setup_questions::run(dir, env, default_port, flag_on, &mut term)
}

/// Runs one device-flow link against `target` at `url` in the terminal, storing the refresh token.
async fn run_link_in_terminal(
    keychain: &Arc<dyn link::TokenStore>,
    url: &str,
    target: link::LinkTarget,
    env: &Env,
    version: &str,
) -> Result<u64, String> {
    if matches!(keychain.get(url), Ok(Some(_))) {
        println!("Already linked to {url}. Linking again replaces that link.");
    }
    println!("Linking to {url}…");
    let flow = link::Link {
        http: reqwest::Client::new(),
        store: keychain.as_ref(),
        machine: link::machine_info(env, version),
        target,
        second: Duration::from_secs(1),
    };
    let shown = |code: &link::DeviceCode| {
        println!("Go to {} and enter {}", code.verification_uri, code.user_code);
        if !code.verification_uri_complete.is_empty() {
            println!("(or open {})", code.verification_uri_complete);
        }
        println!(
            "The code works for {} minutes. Waiting for approval…",
            code.expires_in.div_ceil(60)
        );
    };
    flow.run(url, shown).await.map_err(|e| format!("{e:#}"))
}

/// installers/04-every-agent-on-the-shared-backlog-shared.md: starts the shared backlog's device flow by
/// itself, printing and opening its `/link` page the moment a code is ready. Logs the outcome instead of
/// failing the caller — a failed or expired code waits for the next start, or `retaskable link backlog`.
pub async fn auto_link_backlog(
    links: link::pending::Links,
    keychain: Arc<dyn link::TokenStore>,
    http: reqwest::Client,
    env: Env,
    version: String,
) {
    let shown = |code: &link::DeviceCode| {
        retaskable_agent::terminal_menu::console::say(&format!(
            "Approve the shared backlog: {}",
            code.verification_uri_complete
        ));
        links.set_backlog(Some(link::pending::BacklogWaiting::approve(
            &code.verification_uri_complete,
        )));
        if let Err(e) = link::open_url(&code.verification_uri_complete) {
            tracing::warn!("couldn't open the shared backlog's link page: {e:#}");
        }
    };
    let result = backlog::auto_link(keychain, http, &env, &version, shown).await;
    links.set_backlog(None);
    match result {
        Ok(Some(id)) => tracing::info!("Linked to the shared backlog as agent {id}."),
        Ok(None) => {}
        Err(e) => tracing::warn!("Couldn't link to the shared backlog: {e:#}"),
    }
}
