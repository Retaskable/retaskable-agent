//! The judge session's settings, as the triager builds its own (`triage_session.rs`) but on the scouts' tier and budget.
use std::path::Path;

use super::*;
use crate::config::SandboxMode;

fn placed(engine: EngineId) -> Placement {
    Placement {
        engine,
        bin: "claude".into(),
        cursor: None,
        local: false,
    }
}

fn project() -> ProjectConfig {
    let mut p = ProjectConfig::default();
    p.budget.scout_usd = 0.4;
    p.budget.reviewer_usd = 9.0;
    p.sandbox = SandboxMode::Off;
    p
}

#[test]
fn the_session_has_the_scout_budget_the_tier_and_the_checkout_it_is_given() {
    let cwd = Path::new("base-checkout");
    let cfg = session_config(&project(), &placed(EngineId::Claude), "haiku", cwd, None);
    assert_eq!((cfg.budget_usd, cfg.model.as_str(), cfg.cwd), (0.4, "haiku", cwd));
    assert_eq!(cfg.engine, Some(EngineId::Claude));
    assert!(!cfg.sandbox && cfg.shell && cfg.local.is_none());
    let spec = crate::ci_judge::session_spec(1, "t", "b", "w", &cfg).unwrap();
    assert!(spec.readonly && spec.shell);
    assert_eq!((spec.budget_usd, spec.cwd.as_path()), (0.4, cwd));
}

#[test]
fn the_local_tier_runs_on_the_local_target_as_sonnet_for_claude() {
    let target = LocalTarget {
        base_url: "http://127.0.0.1:1".into(),
        model: "m".into(),
    };
    let cfg = session_config(
        &project(),
        &placed(EngineId::Claude),
        LOCAL,
        Path::new("."),
        Some(target.clone()),
    );
    assert_eq!(cfg.local.as_ref().map(|t| t.model.as_str()), Some("m"));
    assert_eq!(cfg.model, claude_model_for(EngineId::Claude, LOCAL));
}

#[test]
fn the_sandbox_follows_the_projects_setting() {
    let mut p = project();
    p.sandbox = SandboxMode::Auto;
    let cfg = session_config(&p, &placed(EngineId::Claude), "haiku", Path::new("."), None);
    assert_eq!(cfg.sandbox, sandbox_enabled(SandboxMode::Auto, cfg!(windows)));
}

#[test]
fn the_bell_is_logged_not_shown() {
    // There is no title-and-body notification on the agent: the call must not panic or need a tray.
    let host = ClaudeCiWatch::new(
        Default::default(),
        Env::empty(),
        Default::default(),
        LocalModel::off(),
        Sessions::default(),
    );
    host.bell("Pull request #1 is red", "Title (at abc)");
}
