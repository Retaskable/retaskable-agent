//! Linking the CI runner (docs/specs/agent-ci.md section 2, ci-agent-process.md): the same device flow a Coding Agent
//! uses, once, for the shared backlog store, asking for `role: "ci"`. The token goes in the runner's own credential
//! identity, so linking it never replaces the Coding Agent's link on the same computer. It never links to
//! retaskable.com's Worker: it uses only the store.
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

use super::settings;
use crate::backlog;
use crate::env::Env;
use crate::link::{self, BACKLOG_TARGET, DeviceCode, Link, TokenStore};

/// What the store's `/link` page shows and the code request carries as the role of this link.
pub const ROLE: &str = "ci";
/// The OS credential service name of the runner's own link.
pub const KEYCHAIN_SERVICE: &str = "retaskable-ci";
/// Names a file for the token, on a computer with no OS credential store, like `RETASKABLE_CREDENTIALS_FILE`.
pub const CREDENTIALS_FILE_VAR: &str = "RETASKABLE_CI_CREDENTIALS_FILE";

/// Where its refresh token is kept.
pub fn token_store(env: &Env) -> Arc<dyn TokenStore> {
    link::keychain_for(&settings::data_dir(env), env, KEYCHAIN_SERVICE, CREDENTIALS_FILE_VAR)
}

/// This machine and the role the runner asks to be linked as.
pub fn machine(env: &Env, version: &str) -> Value {
    let mut info = link::machine_info(env, version);
    if let Some(o) = info.as_object_mut() {
        o.insert("role".into(), json!(ROLE));
    }
    info
}

/// The shared backlog store's address, checked before it is used.
pub fn store_url(env: &Env) -> Result<String> {
    let url = backlog::url(env);
    link::valid_http_url(&url, "backlog store")?;
    Ok(url)
}

/// Links to the store, showing the code through `say`. A link that is already there is left alone.
pub async fn run(env: &Env, version: &str, say: impl Fn(&str)) -> Result<()> {
    let url = store_url(env)?;
    let store = token_store(env);
    if matches!(store.get(&url), Ok(Some(_))) {
        say(&format!("Already linked to the {} at {url}.", BACKLOG_TARGET.name));
        return Ok(());
    }
    let flow = Link {
        http: reqwest::Client::new(),
        store: store.as_ref(),
        machine: machine(env, version),
        target: BACKLOG_TARGET,
        second: Duration::from_secs(1),
    };
    let shown = |c: &DeviceCode| {
        say(&format!(
            "Approve the CI runner for the {} at {}\nCode: {}",
            BACKLOG_TARGET.name, c.verification_uri_complete, c.user_code
        ));
    };
    flow.run(&url, shown).await?;
    say(&format!("Linked to the {} at {url}.", BACKLOG_TARGET.name));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_machine_asks_to_be_linked_as_ci() {
        let env = Env::from_pairs([("RETASKABLE_MACHINE", "box")]);
        let m = machine(&env, "1.2.3");
        assert_eq!(
            (m["role"].clone(), m["machine"].clone(), m["agentVersion"].clone()),
            (json!("ci"), json!("box"), json!("1.2.3"))
        );
    }

    #[test]
    fn its_data_lives_apart_from_the_coding_agents() {
        let env = Env::from_pairs([("RETASKABLE_DATA_DIR", "d")]);
        assert_eq!(settings::data_dir(&env), PathBuf::from("d").join("ci"));
    }

    #[test]
    fn a_bad_store_address_is_refused_before_anything_is_sent() {
        let env = Env::from_pairs([("RETASKABLE_BACKLOG_URL", "not a url")]);
        assert!(store_url(&env).is_err());
    }
}
