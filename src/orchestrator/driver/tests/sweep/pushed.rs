//! The commands the hub still pushes that the loop settles with the asks its passes already answer
//! (`driver/pushed.rs`): the helper they share. Each module that owns the host's fake (`merge_item`, `ship`, `main_pr`) has its
//! own tests of them beside the row's.
use super::*;
use crate::view::ProjectCommand;

/// Pushes `command` to project "p0" of `rig` and returns the command's answer. The ask it makes is taken on the next pass, and
/// the answer is sent from a task, so the passes come first and the wait is bounded, not a sleep.
pub(in crate::orchestrator::driver::tests) async fn pushed(rig: &mut Rig, command: ProjectCommand) -> Value {
    rig.machine.wanted = true;
    let items = rig.machine.links.item_commands.clone();
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    items.set_out("p0", Some(tx));
    items.projects.push(command);
    for _ in 0..3 {
        rig.machine.pass().await;
    }
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("the command was answered")
        .unwrap()
}
