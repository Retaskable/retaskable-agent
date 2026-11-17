//! `phone_play { serial, confirmed?, uninstall? }` and `phone_stop {}` (`agent/src/commands.ts`, `phone.ts`): Play on phone
//! from the site ([`crate::phone_run`]). Neither needs the orchestrator: the run is the machine's, holding the project's
//! base checkout while it builds and installs. `phone_play` answers once the run has started (its progress is the
//! snapshot's `phone.run`), or is refused with the reason: no `phone` section, a phone that isn't connected with USB debugging
//! allowed, the first install's warning not confirmed, uninstalling without `confirmed`, a run already going, the base
//! checkout busy.
use serde_json::Value;

use super::{Command, ack, allowed_project, answer, nack};
use crate::view::AgentView;

pub(super) fn play(cmd: &Command, project: &str, view: &AgentView, data_dir: &std::path::Path) -> Value {
    let Some(serial) = cmd.args.get("serial").and_then(Value::as_str).filter(|s| !s.is_empty()) else {
        return nack(&cmd.id, "That phone isn't connected with USB debugging allowed.".into());
    };
    let flag = |k: &str| cmd.args.get(k).and_then(Value::as_bool) == Some(true);
    let Some(p) = allowed_project(view, project) else {
        return nack(&cmd.id, "Finish setup first.".to_string());
    };
    answer(
        cmd,
        crate::phone_run::play(project, &p, data_dir, serial, flag("confirmed"), flag("uninstall")),
    )
}

pub(super) fn stop(cmd: &Command) -> Value {
    crate::phone_run::stop();
    ack(&cmd.id)
}
