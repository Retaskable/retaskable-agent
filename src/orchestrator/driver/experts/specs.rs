//! The draft specs a session's turn asked for, filed as `draft_spec` files for the person to approve on the site.
use super::*;

/// Files each one; the history says so, or why one wasn't filed.
pub(super) async fn file(cx: &Cx<'_>, item: &str, s: &Session, specs: Vec<String>) {
    for request in specs {
        let args = crate::intents::draft_wanted::plain_args(&request, &s.started_by);
        match cx.client.draft_queue_args(cx.repo, Some(cx.token), args).await {
            Ok(draft) => cx.note(item, &s.lease, &format!("Draft spec filed: {draft}")).await,
            Err(e) => {
                cx.note(item, &s.lease, &format!("A draft spec wasn't filed: {e}"))
                    .await;
            }
        }
    }
}
