//! What the cloud sees of this computer.
use std::path::Path;
use std::sync::Arc;

use retaskable_agent::env::Env;
use retaskable_agent::{config, hello_settings, lane_pool, link, local_model, view};

/// What the cloud sees of this computer.
#[allow(clippy::too_many_arguments)]
pub fn agent_view(
    cfg: &config::ConfigFile,
    tags: Vec<String>,
    version: &str,
    env: &Env,
    dir: &Path,
    default_port: u16,
    local: &local_model::LocalModel,
    checks: Arc<retaskable_agent::setup_check::SetupCheckRunner>,
) -> view::AgentView {
    let flags = Arc::new(retaskable_agent::flags::Flags::from_env());
    // The snapshot reads the flag synchronously, so something has to keep asking.
    flags.keep_fresh(view::SNAPSHOT_REPORTING_FLAG, std::time::Duration::from_secs(30));
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
        settings: hello_settings::HelloSettings::new(dir.to_path_buf(), env.clone(), default_port, flags),
        live: Default::default(),
        checks,
        owner: Default::default(),
    }
}
