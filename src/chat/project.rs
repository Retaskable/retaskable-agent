//! What the Chat Agent says about a project when it connects, and the id its socket goes by.
use serde_json::{Value, json};

use super::{CHAT_CONTEXT_CAPABILITY, ROLE};
use crate::env::Env;
use crate::link;

/// Limits the projects served to this one repository id (the store's `repoId`), for a person who wants one only, or a
/// test. Unset serves everything `projects_served` answers.
pub const REPO_ID_VAR: &str = "RETASKABLE_CHAT_REPO_ID";

/// The key one project's socket connects as (`?project=`): the kebab of `owner/name`, `^[a-z0-9]+(-[a-z0-9]+)*$` and at
/// most 100 characters. It only names this agent's own hub row; the hub places the project by the hello's `githubRepo`.
pub fn socket_id(github_repo: &str) -> String {
    let kebab: Vec<String> = github_repo
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase)
        .collect();
    let key: String = kebab.join("-").chars().take(100).collect();
    let key = key.trim_end_matches('-');
    if key.is_empty() { "project".into() } else { key.into() }
}

/// The `hello` for one project's socket: the Chat Agent's role and its one capability, no lanes and no settings. The
/// project names only its repository: the Chat Agent has no checkout, so no backlog path to say, and the hub never
/// lets this role write one.
pub fn hello(env: &Env, version: &str, github_repo: &str) -> Value {
    let info = link::machine_info(env, version);
    json!({
        "type": "hello",
        "agentVersion": version,
        "os": info["os"],
        "machine": info["machine"],
        "role": ROLE,
        "capabilities": [CHAT_CONTEXT_CAPABILITY],
        "project": { "githubRepo": github_repo },
        "tags": [],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hello_is_the_chat_roles_with_its_one_capability() {
        let env = Env::from_pairs([("RETASKABLE_MACHINE", "box")]);
        let h = hello(&env, "1.2.3", "o/r");
        assert_eq!(
            (h["type"].clone(), h["role"].clone(), h["machine"].clone()),
            (json!("hello"), json!("chat"), json!("box"))
        );
        assert_eq!(h["capabilities"], json!(["chat_context"]));
        assert_eq!(h["project"], json!({ "githubRepo": "o/r" }));
        assert!(h.get("lanes").is_none() && h.get("settings").is_none());
    }

    #[test]
    fn the_socket_goes_by_the_kebab_of_owner_and_name() {
        assert_eq!(socket_id("Some-Org/my.repo_2"), "some-org-my-repo-2");
        assert_eq!(socket_id(""), "project");
        let valid = |k: &str| {
            k.len() <= 100
                && !k.is_empty()
                && k.split('-')
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
        };
        for repo in [
            "Owner/My.Repo_x",
            "o/r",
            "",
            "--/..",
            &format!("{}/{}", "a".repeat(60), "b-".repeat(50)),
        ] {
            assert!(valid(&socket_id(repo)), "{repo}");
        }
    }
}
