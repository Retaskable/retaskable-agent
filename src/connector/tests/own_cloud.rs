//! The release note's page is the agent's own cloud's to ask for: the first cloud this agent connects to.
use super::*;

#[tokio::test]
async fn a_publish_over_another_clouds_connection_is_refused_and_the_first_clouds_reaches_the_command() {
    let (first_url, _, mut first_rx) = cloud().await;
    let (other_url, _, mut other_rx) = cloud().await;
    let (dir, st) = linked(&first_url);
    let (other_dir, other_st) = linked(&other_url);
    let view = view(&["one"]);
    let first = start_connector(&first_url, st, &view, dir.path());
    let mut a = next(&mut first_rx).await;
    read(&mut a).await;
    read(&mut a).await;
    let other = start_connector(&other_url, other_st, &view, other_dir.path());
    let mut b = next(&mut other_rx).await;
    read(&mut b).await;
    read(&mut b).await;
    for name in ["release_publish", "release_unpublish"] {
        b.ws.send(Message::text(command("c-other", name, json!({})).to_string()))
            .await
            .unwrap();
        let nack = read_not_alive(&mut b).await;
        assert_eq!(nack["type"], "nack");
        assert_eq!(
            nack["reason"],
            "Release notes are published on this agent's own retaskable.com, not this one: ask from there."
        );
    }
    // Over the first cloud's it is the command's own checks that answer (this view runs no work).
    a.ws.send(Message::text(
        command("c-own", "release_publish", json!({})).to_string(),
    ))
    .await
    .unwrap();
    let own = read_not_alive(&mut a).await;
    assert_eq!(own["type"], "nack");
    assert_eq!(
        own["reason"],
        "This computer isn't running work (the orchestrator isn't in claim or runs mode)."
    );
    first.stop(Duration::from_secs(1)).await;
    other.stop(Duration::from_secs(1)).await;
}
