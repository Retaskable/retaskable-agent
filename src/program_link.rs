//! How the Chat Agent and the CI runner are linked (agent-settings.md section 4.6, programs.md): the one place that says
//! each one's role, its own credential identity and what it links to. `retaskable-chat-agent link`, `retaskable-ci link`
//! and the settings page's **Link** button (`page_host::link`) all read it, so the token the page writes is the one the
//! program's own command would, in the same store with the same permissions, and the running program finds it with no
//! restart (`link::DeviceTokens` reads its store on every refresh).
//!
//! A neutral module (programs.md): it names neither program's own code.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde_json::{Value, json};

use crate::backlog;
use crate::config::ConfigFile;
use crate::env::Env;
use crate::link::{self, BACKLOG_TARGET, CLOUD_TARGET, LinkTarget, TokenStore};
use crate::paths;
use crate::program_status::{CHAT_AGENT, CI_RUNNER};

/// One program's link.
pub struct Spec {
    /// `program_status`'s id for it.
    pub program: &'static str,
    /// What the page calls it.
    pub label: &'static str,
    /// The `role` its code request carries, which the store's `/link` page shows.
    pub role: &'static str,
    /// The OS credential service its tokens are kept under: its own identity, never the Coding Agent's.
    pub service: &'static str,
    /// The variable that names a file for its tokens instead (a container has no OS credential store).
    pub file_var: &'static str,
    /// Whether it links to retaskable.com's Worker as well as the shared backlog store.
    pub cloud: bool,
    /// The terminal command that does the same, for a program the page can't link.
    pub command: &'static str,
}

pub const CI: Spec = Spec {
    program: CI_RUNNER,
    label: "CI runner",
    role: "ci",
    service: "retaskable-ci",
    file_var: "RETASKABLE_CI_CREDENTIALS_FILE",
    cloud: false,
    command: "retaskable-ci link",
};

pub const CHAT: Spec = Spec {
    program: CHAT_AGENT,
    label: "Chat agent",
    role: "chat",
    service: "retaskable-chat",
    file_var: "RETASKABLE_CHAT_CREDENTIALS_FILE",
    cloud: true,
    command: "retaskable-chat-agent link",
};

/// The spec of the program with id `program`.
pub fn spec(program: &str) -> Option<&'static Spec> {
    [&CI, &CHAT].into_iter().find(|s| s.program == program)
}

/// One link to make: what it is called, where, and which device endpoints.
pub struct Step {
    pub label: &'static str,
    pub url: String,
    pub target: LinkTarget,
}

impl Spec {
    /// Its own folder inside the data folder `root`.
    pub fn folder(&self, root: &Path) -> PathBuf {
        if self.program == CI_RUNNER {
            paths::ci_dir(root)
        } else {
            paths::chat_dir(root)
        }
    }

    /// Where its refresh tokens are kept, as its own command finds them.
    pub fn store(&self, root: &Path, env: &Env) -> Arc<dyn TokenStore> {
        link::keychain_for(&self.folder(root), env, self.service, self.file_var)
    }

    /// This machine and the role the program asks to be linked as.
    pub fn machine(&self, env: &Env, version: &str) -> Value {
        let mut info = link::machine_info(env, version);
        if let Some(o) = info.as_object_mut() {
            o.insert("role".into(), json!(self.role));
        }
        info
    }

    /// What it links to, in order, each address checked before anything is sent. The Worker is `RETASKABLE_CLOUD_URL`,
    /// else retaskable.com: never the Coding Agent's config.json, which the Chat Agent doesn't read.
    pub fn steps(&self, env: &Env) -> Result<Vec<Step>> {
        let mut steps = Vec::new();
        if self.cloud {
            let url = link::cloud_url(&ConfigFile::default(), env, None)?;
            steps.push(Step {
                label: "retaskable.com",
                url,
                target: CLOUD_TARGET,
            });
        }
        let url = backlog::url(env);
        link::valid_http_url(&url, "backlog store")?;
        steps.push(Step {
            label: "the shared backlog store",
            url,
            target: BACKLOG_TARGET,
        });
        Ok(steps)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_program_links_as_its_own_role_in_its_own_credential_identity() {
        let env = Env::from_pairs([("RETASKABLE_MACHINE", "box")]);
        assert_eq!(CI.machine(&env, "1.0.0")["role"], json!("ci"));
        assert_eq!(CHAT.machine(&env, "1.0.0")["role"], json!("chat"));
        assert_ne!(CI.service, CHAT.service);
        assert_ne!(CI.service, "retaskable", "never the Coding Agent's");
        assert_ne!(CI.file_var, CHAT.file_var);
    }

    #[test]
    fn the_chat_agent_links_the_worker_then_the_store_and_the_ci_runner_only_the_store() {
        let env = Env::from_pairs([
            ("RETASKABLE_CLOUD_URL", "http://cloud.example/"),
            ("RETASKABLE_BACKLOG_URL", "http://store.example"),
        ]);
        let urls = |spec: &Spec| -> Vec<String> { spec.steps(&env).unwrap().into_iter().map(|s| s.url).collect() };
        assert_eq!(urls(&CHAT), ["http://cloud.example", "http://store.example"]);
        assert_eq!(urls(&CI), ["http://store.example"]);
    }

    #[test]
    fn a_program_is_found_by_its_status_id_and_the_code_agent_is_not_one() {
        assert_eq!(spec("ci-runner").map(|s| s.role), Some("ci"));
        assert_eq!(spec("chat-agent").map(|s| s.role), Some("chat"));
        assert!(spec("code-agent").is_none());
    }
}
