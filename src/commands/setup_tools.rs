//! `install_tool { tool }` and `tool_sign_in { tool }` (setup-in-app.md sections 5.1 and 6.1, `agent/src/commands.ts`): the
//! machine's own, so a computer with no project yet takes them too. Each runs at once, with no one to allow it. An install
//! `ack`s once it's done (git on macOS once its dialog shows) and `nack`s with the failure's one line; a sign-in `ack`s once
//! it has started, and how it ended goes up in `hello`.
use serde_json::{Value, json};

use super::{Command, PENDING, ack, nack};
use crate::env::Env;
use crate::view::AgentView;

/// The tool the command names: exactly `{ tool }`, one of the fixed names, and nothing else.
fn tool_of<'a>(cmd: &'a Command, allowed: &[&str]) -> Result<&'a str, String> {
    let args = cmd.args.as_object();
    let tool = args.and_then(|a| a.get("tool")).and_then(Value::as_str);
    match tool {
        Some(t) if args.is_some_and(|a| a.len() == 1) && allowed.contains(&t) => Ok(t),
        _ => Err(format!("{} takes only {{ tool: {} }}.", cmd.name, allowed.join(" | "))),
    }
}

pub fn install_tool(cmd: &Command, project: &str, view: &AgentView) -> Value {
    let Some(setup) = view.setup.clone() else {
        return nack(&cmd.id, "This agent can't install tools from retaskable.com.".into());
    };
    let tool = match tool_of(cmd, &crate::setup_tools::TOOLS) {
        Ok(t) => t.to_string(),
        Err(why) => return nack(&cmd.id, why),
    };
    if let Some(why) = setup.install_refusal(&tool) {
        return nack(&cmd.id, why);
    }
    let (id, project, commands) = (cmd.id.clone(), project.to_string(), view.item_commands.clone());
    tokio::task::spawn_blocking(move || {
        let answer = match setup.install(&tool) {
            Ok(()) => ack(&id),
            Err(why) => nack(&id, why),
        };
        commands.answer(&project, answer);
    });
    json!({ "type": PENDING, "id": cmd.id })
}

pub fn tool_sign_in(cmd: &Command, view: &AgentView) -> Value {
    let Some(setup) = view.setup.clone() else {
        return nack(&cmd.id, "This agent can't sign in from retaskable.com.".into());
    };
    let tool = match tool_of(cmd, &["gh", "claude"]) {
        Ok(t) => t,
        Err(why) => return nack(&cmd.id, why),
    };
    match setup.sign_in(tool, &Env::from_process()) {
        Ok(()) => ack(&cmd.id),
        Err(why) => nack(&cmd.id, why),
    }
}
