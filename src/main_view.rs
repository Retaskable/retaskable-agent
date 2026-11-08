//! The view of this computer the binary hands the cloud link.
use retaskable_agent::env::Env;
use retaskable_agent::{config, hello_settings, lane_pool, link, local_model, view};
use std::path::Path;
use std::sync::Arc;

/// What the cloud sees of this computer.
pub fn agent_view(
    cfg: &config::ConfigFile,
    tags: Vec<String>,
    version: &str,
    env: &Env,
    dir: &Path,
    default_port: u16,
    local: &local_model::LocalModel,
) -> view::AgentView {
    view::AgentView {
        machine: link::machine_name(env),
        tags,
        projects: cfg.projects.iter().map(view::ProjectView::from).collect(),
        version: version.to_string(),
        lanes: config::lanes(cfg, env),
        chore_lane: cfg.machine.chore_lane,
        lane_sizes: lane_pool::LaneSizes::default(),
        paused: view::PauseState::default(),
        item_commands: Default::default(),
        work: Default::default(),
        restart: Default::default(),
        settings: hello_settings::HelloSettings::new(
            dir.to_path_buf(),
            env.clone(),
            default_port,
            Arc::new(retaskable_agent::flags::Flags::from_env()),
        ),
        local: local.clone(),
    }
}
