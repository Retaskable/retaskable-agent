//! Filling a prompt template: `render_prompt` fills the `{{name}}` placeholders and `project_words`
//! gives the per-project-type words the templates share (agent/src/prompts.ts). A leaf every prompt
//! builder (ideas, implementer, reviewer, scouts, triage, playtest groups, release notes) depends on.
use std::collections::HashMap;

use anyhow::Result;
use regex::Regex;

use crate::config::ProjectType;

/// The words `{{projectKind}}`/`{{projectNoun}}`/`{{projectUser}}`/`{{projectExamples}}`/`{{projectSteps}}`
/// fill in, one set per project type (shared/projectType.ts's `WORDS` + agent/src/prompts.ts's
/// `PROMPT_WORDS`, combined).
pub struct ProjectWords {
    pub kind: &'static str,
    pub noun: &'static str,
    pub user: &'static str,
    /// `PROMPT_WORDS[t].examples`: the kinds of player/user-visible change `{{playtest}}` asks about.
    pub examples: &'static str,
    /// `PROMPT_WORDS[t].steps`: how the playtest's `steps` reach the change, from launch.
    pub steps: &'static str,
    /// `PROMPT_WORDS[t].parts`: an example of "the same part of {{projectNoun}}", for the
    /// playtest-groups prompt (06.4-playtests.md's "Grouped into sessions").
    pub parts: &'static str,
}

pub fn project_words(project_type: ProjectType) -> ProjectWords {
    match project_type {
        ProjectType::Game => ProjectWords {
            kind: "a game",
            noun: "the game",
            user: "a player",
            examples: "new behaviour, visuals, controls, feel, balance",
            steps: "from launch",
            parts: "a screen or mode, like the menus, the HUD, a core mechanic or the opening",
        },
        ProjectType::Web => ProjectWords {
            kind: "a web app",
            noun: "the web app",
            user: "a user",
            examples: "new behaviour, pages, layout, flows, messages",
            steps: "from launch (open the web app, then where to click)",
            parts: "a page or flow, like sign-in, settings or the dashboard",
        },
        ProjectType::Server => ProjectWords {
            kind: "a server or API",
            noun: "the server",
            user: "a client or caller",
            examples: "new behaviour, responses, output, errors, flags",
            steps: "from launch (start the server, then the requests to send, e.g. `curl localhost:8787/api/me`)",
            parts: "an endpoint group, like auth, users or billing",
        },
        ProjectType::Cli => ProjectWords {
            kind: "a command-line program or library",
            noun: "the program",
            user: "a user",
            examples: "new behaviour, responses, output, errors, flags",
            steps: "from launch (the commands to run)",
            parts: "a command and its flags",
        },
        ProjectType::Other => ProjectWords {
            kind: "an app",
            noun: "the app",
            user: "a user",
            examples: "new behaviour, visuals, output, flows, messages",
            steps: "from launch",
            parts: "a screen, feature or command",
        },
    }
}

/// `render()` (agent/src/prompts.ts): fills `{{name}}` placeholders, failing on one the caller didn't
/// supply rather than leaving it in or silently dropping it.
pub fn render_prompt(template: &str, vars: &[(&str, String)]) -> Result<String> {
    let map: HashMap<&str, &str> = vars.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let placeholder = Regex::new(r"\{\{(\w+)\}\}").unwrap();
    let mut missing: Option<String> = None;
    let out = placeholder.replace_all(template, |caps: &regex::Captures| match map.get(&caps[1]) {
        Some(value) => value.to_string(),
        None => {
            missing = Some(caps[1].to_string());
            String::new()
        }
    });
    match missing {
        Some(key) => anyhow::bail!("prompt is missing the {{{{{key}}}}} variable"),
        None => Ok(out.into_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_words_covers_every_project_type() {
        for t in [
            ProjectType::Game,
            ProjectType::Web,
            ProjectType::Server,
            ProjectType::Cli,
            ProjectType::Other,
        ] {
            let words = project_words(t);
            assert!(!words.kind.is_empty());
            assert!(!words.noun.is_empty());
            assert!(!words.user.is_empty());
            assert!(!words.parts.is_empty());
        }
    }

    #[test]
    fn render_prompt_fills_every_placeholder() {
        let out = render_prompt(
            "Hello {{name}}, welcome to {{place}}.",
            &[("name", "A".to_string()), ("place", "B".to_string())],
        )
        .unwrap();
        assert_eq!(out, "Hello A, welcome to B.");
    }

    #[test]
    fn render_prompt_fails_on_a_placeholder_with_no_value() {
        let err = render_prompt("Hello {{name}}.", &[]).unwrap_err();
        assert!(err.to_string().contains("name"));
    }
}
