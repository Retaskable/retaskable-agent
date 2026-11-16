//! A session that carries on a Claude Code session (docs/specs/expert-chat.md section 3.10): its branch starts at the
//! tip of the Claude Code session's, the session is noted in the store (an id and a title, no path), and its first
//! turn opens with the handover. A Claude Code session is carried on once: the person's open session that has it takes
//! the message when they continue it again.
use super::*;
use crate::backlog::expert::ExpertFrom;

/// The open session `by` started that carries on Claude Code session `session`, if there is one.
pub(super) fn open_session(cx: &Cx<'_>, ex: &Experts, by: &str, session: &str) -> Option<String> {
    (ex.sessions.iter())
        .find(|(id, s)| {
            s.started_by == by
                && cx
                    .queue
                    .continued_from(id)
                    .is_some_and(|from| from.session_id == session)
        })
        .map(|(id, _)| id.clone())
}

/// Starts the session's branch at the tip of the Claude Code session's (its lane then picks the work up as a parked
/// session's), says in the history where it starts from, and notes the Claude Code session in the store.
pub(super) async fn carry_on(cx: &Cx<'_>, s: &mut Session, id: &str, from: &Continued) {
    match cx.host.carry_on(cx.project, from, &s.branch).await {
        Ok(()) => {
            s.from_branch = true;
            let on = from.branch.as_deref().unwrap_or("its branch");
            cx.note(id, &s.lease, &format!("Carries on from {on} in {}", from.folder))
                .await;
        }
        Err(why) => {
            let base = &cx.project.base_branch;
            cx.note(id, &s.lease, &format!("Starts from {base}: {why}")).await;
        }
    }
    let note = ExpertFrom {
        session_id: from.session_id.clone(),
        title: from.title.clone(),
        branch: from.branch.clone(),
    };
    if !cx.queue.set_continued(id, note).await {
        let why = "Its Claude Code session couldn't be noted in the store yet: it is sent when the store is back";
        cx.note(id, &s.lease, why).await;
    }
    s.handover_from = Some(from.session_id.clone());
}

/// The message a turn starts with: the handover around `text` while the session has none answered yet and continues a
/// Claude Code session whose file is still here, else `text`.
pub(super) async fn first_message(cx: &Cx<'_>, s: &Session, text: String) -> String {
    let Some(from) = s.handover_from.as_deref().filter(|_| s.session_id.is_none()) else {
        return text;
    };
    cx.host.handover(from, &text).await.unwrap_or(text)
}
