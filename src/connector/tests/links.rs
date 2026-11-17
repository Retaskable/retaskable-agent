//! The hello sent again when a link's state changes (the clouds' and the backlog store's states ride in `computer.secrets`).
use super::*;

use crate::computer_view::ComputerContext;
use crate::link::pending::{BacklogWaiting, Links};
use crate::secrets::fakes::MemStore;

fn backlog_state(hello: &Value) -> Value {
    let row = hello["computer"]["secrets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "link" && s["key"].as_str().is_some_and(|k| k.starts_with("backlog store")))
        .cloned()
        .expect("a backlog store row");
    row["state"].clone()
}

#[tokio::test]
async fn hello_is_sent_again_when_a_links_state_changes_and_not_when_nothing_it_says_did() {
    let conf = tempfile::tempdir().unwrap();
    std::fs::write(conf.path().join("config.json"), json!({ "version": 2 }).to_string()).unwrap();
    let keychain: Arc<dyn TokenStore> = Arc::new(MemStore::with(&[]));
    let env = crate::env::Env::empty();
    let links = Links::new(keychain.clone(), reqwest::Client::new(), &env, "1");
    let settings = crate::hello_settings::HelloSettings::new(conf.path().to_path_buf(), env);
    settings.set_context(ComputerContext {
        default_port: 4700,
        build: None,
        keychain,
        links: links.clone(),
    });
    let mut v = view(&["one"]);
    Arc::get_mut(&mut v).unwrap().settings = settings;
    let (url, _, mut rx) = cloud().await;
    let (dir, store) = linked(&url);
    let c = start_connector(&url, store, &v, dir.path());
    let mut a = next(&mut rx).await;
    assert_eq!(backlog_state(&read(&mut a).await), "not linked");
    assert_eq!(read(&mut a).await["type"], "snapshot");

    links.set_backlog(Some(BacklogWaiting::approve("https://store.test/approve")));
    let hello = read_not_alive(&mut a).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(backlog_state(&hello), "waiting");

    // The same link set again says nothing new: the next thing sent is a command's answer.
    links.set_backlog(Some(BacklogWaiting::approve("https://store.test/approve")));
    said(&mut a, command("c1", "start_over", json!({}))).await;
    assert_eq!(read_not_alive(&mut a).await["id"], "c1");

    links.set_backlog(None);
    assert_eq!(backlog_state(&read_not_alive(&mut a).await), "not linked");
    c.stop(Duration::from_secs(1)).await;
}
