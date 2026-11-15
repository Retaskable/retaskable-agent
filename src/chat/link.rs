//! Linking the Chat Agent (chat-agent-process.md section 3): the same device flow a Coding Agent uses, twice, once for
//! retaskable.com's Worker and once for the shared backlog store, each asking for `role: "chat"`. The tokens go in the
//! Chat Agent's own credential identity, so linking it never replaces the Coding Agent's link on the same computer.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

use super::{CREDENTIALS_FILE_VAR, KEYCHAIN_SERVICE, ROLE};
use crate::backlog;
use crate::config;
use crate::env::Env;
use crate::link::{self, BACKLOG_TARGET, CLOUD_TARGET, DeviceCode, DeviceTokens, Link, LinkTarget, TokenStore};
use crate::paths;

/// The Chat Agent's own folder inside the data folder.
pub fn data_dir(env: &Env) -> PathBuf {
    paths::chat_dir(&root(env))
}

/// The data folder all three programs share, found the way the code agent finds it (`paths::data_dir`).
pub fn root(env: &Env) -> PathBuf {
    paths::current_data_dir(env)
}

/// `data_dir` for a given executable, so a test can say where its `build.json` is.
pub fn data_dir_for(env: &Env, exe: Option<&std::path::Path>) -> PathBuf {
    paths::chat_dir(&paths::data_dir_for_exe(env, exe))
}

/// Where its refresh tokens are kept.
pub fn token_store(env: &Env) -> Arc<dyn TokenStore> {
    link::keychain_for(&data_dir(env), env, KEYCHAIN_SERVICE, CREDENTIALS_FILE_VAR)
}

/// What the `/link` page shows and the code request carries: this machine, and the role the agent asks to be linked as.
pub fn machine(env: &Env, version: &str) -> Value {
    let mut info = link::machine_info(env, version);
    if let Some(o) = info.as_object_mut() {
        o.insert("role".into(), json!(ROLE));
    }
    info
}

/// The two addresses this agent links to: the Worker and the shared backlog store, each checked before it is used.
pub fn urls(env: &Env) -> Result<(String, String)> {
    let cfg = config::load(&root(env))?;
    let cloud = link::cloud_url(&cfg, env, None)?;
    let store = backlog::url(env);
    link::valid_http_url(&store, "backlog store")?;
    Ok((cloud, store))
}

/// One token holder per target, so the connector and the store each refresh their own.
pub fn tokens(env: &Env, url: String, target: LinkTarget, http: reqwest::Client) -> DeviceTokens {
    DeviceTokens::new(url, target, token_store(env), http)
}

/// Links both, one after the other, showing each code through `say`. A link that is already there is left alone.
pub async fn run(env: &Env, version: &str, say: impl Fn(&str)) -> Result<()> {
    let (cloud, store_url) = urls(env)?;
    let store = token_store(env);
    let http = reqwest::Client::new();
    for (url, target) in [(cloud, CLOUD_TARGET), (store_url, BACKLOG_TARGET)] {
        if matches!(store.get(&url), Ok(Some(_))) {
            say(&format!("Already linked to the {} at {url}.", target.name));
            continue;
        }
        let flow = Link {
            http: http.clone(),
            store: store.as_ref(),
            machine: machine(env, version),
            target,
            second: Duration::from_secs(1),
        };
        let shown = |c: &DeviceCode| {
            say(&format!(
                "Approve the Chat Agent for the {} at {}\nCode: {}",
                target.name, c.verification_uri_complete, c.user_code
            ));
        };
        flow.run(&url, shown).await?;
        say(&format!("Linked to the {} at {url}.", target.name));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_machine_asks_to_be_linked_as_chat() {
        let env = Env::from_pairs([("RETASKABLE_MACHINE", "box")]);
        let m = machine(&env, "1.2.3");
        assert_eq!(
            (m["role"].clone(), m["machine"].clone(), m["agentVersion"].clone()),
            (json!("chat"), json!("box"), json!("1.2.3"))
        );
    }

    #[test]
    fn its_data_lives_apart_from_the_coding_agents() {
        let env = Env::from_pairs([("RETASKABLE_DATA_DIR", "d")]);
        assert_eq!(data_dir(&env), PathBuf::from("d").join("chat"));
    }

    #[test]
    fn a_bad_store_address_is_refused_before_anything_is_sent() {
        let env = Env::from_pairs([
            ("RETASKABLE_BACKLOG_URL", "not a url"),
            ("RETASKABLE_DATA_DIR", "no-such-dir"),
        ]);
        assert!(urls(&env).is_err());
    }

    #[test]
    fn the_chat_agents_data_folder_agrees_with_the_code_agents_rule() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("agent");
        std::fs::write(
            tmp.path().join("build.json"),
            r#"{"os":"linux","version":"1.2.3","commit":"abc","environment":"production"}"#,
        )
        .unwrap();
        for env in [
            Env::from_pairs([("RETASKABLE_DATA_DIR", "d"), ("HOME", "/h")]),
            Env::from_pairs([("HOME", "/h"), ("LOCALAPPDATA", "/h/lad")]),
        ] {
            let root = crate::paths::data_dir_for_exe(&env, Some(&exe));
            assert_eq!(data_dir_for(&env, Some(&exe)), root.join("chat"));
        }
        // No build.json: a checkout, as the code agent sees it.
        let none = tmp.path().join("none").join("agent");
        let env = Env::empty();
        assert_eq!(
            data_dir_for(&env, Some(&none)),
            crate::paths::data_dir(&env, None).join("chat")
        );
    }
}
