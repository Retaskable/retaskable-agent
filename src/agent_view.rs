//! What the cloud sees of this computer.
use std::path::Path;
use std::sync::Arc;

use retaskable_agent::env::Env;
use retaskable_agent::{config, hello_settings, lane_pool, link, local_model, view};

/// What the cloud sees of this computer.
#[allow(clippy::too_many_arguments)]
pub fn agent_view(
    cfg: &config::ConfigFile,
    tags: crate::tags::LiveTags,
    version: &str,
    env: &Env,
    dir: &Path,
    local: &local_model::LocalModel,
    checks: Arc<retaskable_agent::setup_check::SetupCheckRunner>,
) -> view::AgentView {
    let settings = hello_settings::HelloSettings::new(dir.to_path_buf(), env.clone());
    let item_commands = view::ItemCommands::default();
    // Allowed to release also approves a project's rollout commands.
    let unasked = settings.clone();
    item_commands.gates().set_releases(move |p| unasked.rollout_unasked(p));
    view::AgentView {
        machine: link::machine_name(env),
        tags,
        projects: cfg.projects.iter().map(view::ProjectView::from).collect(),
        version: version.to_string(),
        lanes: config::lanes(cfg, env),
        chore_lane: cfg.machine.chore_lane,
        local: local.clone(),
        lane_sizes: lane_pool::LaneSizes::default(),
        paused: view::PauseState::persisted(&dir.join("paused.json")),
        store_paused: Default::default(),
        item_commands,
        work: Default::default(),
        restart: Default::default(),
        settings: settings.clone(),
        live: Default::default(),
        setup: Some(retaskable_agent::setup_tools::Setup::new(
            checks.clone(),
            settings,
            dir.to_path_buf(),
        )),
        checks,
        owner: Default::default(),
        github: Default::default(),
        backlog_link: Default::default(),
        add_project: Default::default(),
    }
}
