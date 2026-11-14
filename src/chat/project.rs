//! What the Chat Agent says about a project when it connects, and which store repository that project is.
use serde_json::{Value, json};

use super::{CHAT_CONTEXT_CAPABILITY, ROLE};
use crate::config::ProjectConfig;
use crate::env::Env;
use crate::link;

/// Names the repository id outright, for a project `gh` can't look up (or a test).
pub const REPO_ID_VAR: &str = "RETASKABLE_CHAT_REPO_ID";

/// The `hello` for one project's socket: the Chat Agent's role and its one capability, no lanes and no settings.
pub fn hello(env: &Env, version: &str, project: &ProjectConfig) -> Value {
    let info = link::machine_info(env, version);
    json!({
        "type": "hello",
        "agentVersion": version,
        "os": info["os"],
        "machine": info["machine"],
        "role": ROLE,
        "capabilities": [CHAT_CONTEXT_CAPABILITY],
        "project": { "githubRepo": project.github_repo, "backlogPath": project.backlog_path },
        "tags": [],
    })
}

/// `owner/name` and nothing else, so it can only ever be one `gh api` path.
fn valid_repo(repo: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
    };
    repo.split_once('/').is_some_and(|(owner, name)| ok(owner) && ok(name))
}

/// The numeric GitHub id the shared store keys a project by. `gh api repos/<repo>` reads repository metadata only,
/// never code. A project with no GitHub repository fails closed: the Chat Agent has no other way to place it.
pub async fn repo_id(env: &Env, github_repo: &str) -> Result<u64, String> {
    if let Some(id) = env.get(REPO_ID_VAR).and_then(|v| v.trim().parse::<u64>().ok()) {
        return Ok(id);
    }
    if !valid_repo(github_repo) {
        return Err(format!(
            "The project has no GitHub repository ({github_repo:?}), so the Chat Agent can't place it in the shared backlog."
        ));
    }
    let out = tokio::process::Command::new("gh")
        .args(["api", &format!("repos/{github_repo}"), "--jq", ".id"])
        .output()
        .await
        .map_err(|e| format!("Couldn't run gh to look up {github_repo}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "gh couldn't look up {github_repo}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|_| format!("gh gave no repository id for {github_repo}."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hello_is_the_chat_roles_with_its_one_capability() {
        let env = Env::from_pairs([("RETASKABLE_MACHINE", "box")]);
        let p = ProjectConfig {
            github_repo: "o/r".into(),
            backlog_path: "backlog".into(),
            ..Default::default()
        };
        let h = hello(&env, "1.2.3", &p);
        assert_eq!(
            (h["type"].clone(), h["role"].clone(), h["machine"].clone()),
            (json!("hello"), json!("chat"), json!("box"))
        );
        assert_eq!(h["capabilities"], json!(["chat_context"]));
        assert_eq!(h["project"], json!({ "githubRepo": "o/r", "backlogPath": "backlog" }));
        assert!(h.get("lanes").is_none() && h.get("settings").is_none());
    }

    #[test]
    fn only_owner_slash_name_is_looked_up() {
        for good in ["o/r", "Some-Org/my.repo_2"] {
            assert!(valid_repo(good), "{good}");
        }
        for bad in ["", "r", "o/", "/r", "o/r/x", "-o/r", "o/--jq", "o r/x", "o/r;x"] {
            assert!(!valid_repo(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn the_id_can_be_named_and_a_project_without_a_repo_fails_closed() {
        let named = Env::from_pairs([(REPO_ID_VAR, " 42 ")]);
        assert_eq!(repo_id(&named, "").await, Ok(42));
        let none = Env::from_pairs::<&str, &str>([]);
        let err = repo_id(&none, "").await.unwrap_err();
        assert!(err.contains("no GitHub repository"));
    }
}
