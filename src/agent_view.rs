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
    default_port: u16,
    local: &local_model::LocalModel,
    checks: Arc<retaskable_agent::setup_check::SetupCheckRunner>,
) -> view::AgentView {
    view::AgentView {
        machine: link::machine_name(env),
        tags,
        projects: cfg.projects.iter().map(view::ProjectView::from).collect(),
        version: version.to_string(),
        lanes: config::lanes(cfg, env),
        chore_lane: cfg.machine.chore_lane,
        local: local.clone(),
        lane_sizes: lane_pool::LaneSizes::default(),
        paused: view::PauseState::default(),
        store_paused: Default::default(),
        item_commands: Default::default(),
        work: Default::default(),
        restart: Default::default(),
        settings: hello_settings::HelloSettings::new(dir.to_path_buf(), env.clone(), default_port),
        live: Default::default(),
        checks,
        owner: Default::default(),
    }
}
