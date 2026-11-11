//! The snapshot's `commandsWaiting`, sent again when what waits changes.
use super::*;
use crate::backlog::settings::StoreSettings;

fn store(version: u64, play: &str) -> StoreSettings {
    serde_json::from_value(json!({ "version": version, "commandsVersion": version, "values": { "playCommand": play } }))
        .unwrap()
}

#[tokio::test]
async fn a_new_waiting_version_and_its_approval_each_send_a_snapshot() {
    let project = "cmd-wait";
    let (url, _, mut rx) = cloud().await;
    let (dir, st) = linked(&url);
    let view = view(&[project]);
    let gates = view.item_commands.gates();
    gates.open(dir.path(), project);
    let c = start_connector(&url, st, &view, dir.path());
    let mut first = next(&mut rx).await;
    read(&mut first).await; // hello
    let snap = read(&mut first).await;
    assert!(snap["state"].get("commandsWaiting").is_none());
    let file = crate::config::ProjectConfig::default();
    gates.consider(project, Some(&store(1, "go")), &file).unwrap();
    let again = read_not_alive(&mut first).await;
    assert_eq!(again["type"], "snapshot");
    assert_eq!(again["state"]["commandsWaiting"]["version"], 1);
    gates.approve(project, 1).unwrap();
    let cleared = read_not_alive(&mut first).await;
    assert_eq!(cleared["type"], "snapshot");
    assert!(cleared["state"].get("commandsWaiting").is_none());
    c.stop(Duration::from_secs(1)).await;
}
