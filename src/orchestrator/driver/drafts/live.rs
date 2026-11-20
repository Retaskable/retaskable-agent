//! A plan's live details while the drafter runs (plan-live-details.md section 2, plan-stage.md section 3): about once a second,
//! the lines of the session's transcript (`drafter.jsonl`) are read for what it has done so far, and when that changed the
//! plan's details get `progress` (lines) and `pieces`, so the page shows the plan taking shape. Plain code from the transcript, at
//! most once a second, only while the plan is being drafted; both are cleared when the run ends, whatever the outcome. A write
//! the store refuses is only logged: this is a view, never a step.
use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value};

use crate::backlog::Client;
use crate::drafts::progress::{Live, scan};

const EVERY: Duration = Duration::from_secs(1);

/// Where the plan's details are written: the store, the plan, the lease it is held by, and the transcript to read.
pub(super) struct Details<'a> {
    pub client: &'a Client,
    pub repo: u64,
    pub token: &'a str,
    pub id: &'a str,
    pub lease: &'a str,
    pub transcript: PathBuf,
}

/// Runs `work` (the drafter's session) and keeps the plan's progress while it does.
pub(super) async fn watched<F: Future>(ctx: &Details<'_>, work: F) -> F::Output {
    watched_every(ctx, EVERY, work).await
}

async fn watched_every<F: Future>(ctx: &Details<'_>, every: Duration, work: F) -> F::Output {
    let file = &ctx.transcript;
    let mut seen: Vec<(i64, Value)> = Vec::new();
    let mut sent = Live::default();
    let mut ticks = tokio::time::interval(every);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticks.tick().await;
    tokio::pin!(work);
    let out = loop {
        tokio::select! {
            out = &mut work => break out,
            _ = ticks.tick() => {
                let now = crate::telemetry::now_secs();
                let text = std::fs::read_to_string(file).unwrap_or_default();
                let lines: Vec<&str> = text.lines().collect();
                // A new run writes the file afresh: what was seen of the last one goes.
                if lines.len() < seen.len() {
                    seen.clear();
                }
                for line in &lines[seen.len()..] {
                    if let Ok(v) = serde_json::from_str::<Value>(line) {
                        seen.push((now, v));
                    } else {
                        // A line still being written is read again next time.
                        break;
                    }
                }
                let live = scan(&seen);
                if live != sent && !live.is_empty() {
                    patch(ctx, Some(&live)).await;
                    sent = live;
                }
            }
        }
    };
    if !sent.is_empty() {
        patch(ctx, None).await;
    }
    out
}

/// Writes `progress` and `pieces` (cleared with `None`) over the version read, again once after a stale one.
async fn patch(ctx: &Details<'_>, live: Option<&Live>) {
    let id = ctx.id;
    let mut map = Map::new();
    map.insert("progress".into(), live.map_or(Value::Null, Live::progress));
    map.insert("pieces".into(), live.map_or(Value::Null, Live::pieces));
    for _ in 0..2 {
        let record = match ctx.client.draft_details_get(ctx.repo, Some(ctx.token), id).await {
            Ok(r) => r,
            Err(e) => return tracing::debug!("{id}: reading the plan's details for its progress: {e}"),
        };
        let sent = (ctx.client)
            .draft_details_report(ctx.repo, Some(ctx.token), (id, ctx.lease), record.version, &map)
            .await;
        match sent {
            Err(e) if e.code() == Some("conflict") => continue,
            Err(e) => return tracing::debug!("{id}: writing the plan's progress: {e}"),
            Ok(_) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn the_plans_details_get_progress_and_pieces_while_it_drafts_and_lose_them_when_it_ends() {
        let (base, seen) = crate::backlog::tests::store(200, json!({ "value": { "version": 3, "details": {} } })).await;
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("drafter.jsonl");
        let client = Client::new(base);
        let details = Details {
            client: &client,
            repo: 1,
            token: "tok",
            id: "d1",
            lease: "L1",
            transcript: transcript.clone(),
        };
        let wrote = json!({ "type": "assistant", "message": { "content": [
            { "type": "tool_use", "name": "Write", "input": { "file_path": "docs/specs/dark-mode.md", "content": "# Dark mode\nx" } }] } });
        let work = async {
            std::fs::write(&transcript, format!("{wrote}\n")).unwrap();
            tokio::time::sleep(Duration::from_millis(150)).await;
        };
        watched_every(&details, Duration::from_millis(30), work).await;
        let reports: Vec<Value> = (seen.lock().unwrap().iter())
            .filter(|b| b["op"] == "draft_details_report")
            .map(|b| b["args"].clone())
            .collect();
        assert_eq!(reports.len(), 2, "{reports:?}");
        assert_eq!(reports[0]["token"], "L1");
        assert_eq!(reports[0]["baseVersion"], 3);
        assert_eq!(reports[0]["patch"]["progress"][0]["line"], "Writing Dark mode");
        assert_eq!(reports[0]["patch"]["pieces"], json!([{ "title": "Dark mode" }]));
        assert_eq!(reports[1]["patch"], json!({ "progress": null, "pieces": null }));
    }

    #[tokio::test]
    async fn a_run_that_did_nothing_with_the_specs_writes_nothing() {
        let (base, seen) = crate::backlog::tests::store(200, json!({ "value": null })).await;
        let client = Client::new(base);
        let details = Details {
            client: &client,
            repo: 1,
            token: "tok",
            id: "d1",
            lease: "L1",
            transcript: PathBuf::from("/nonexistent/drafter.jsonl"),
        };
        watched_every(
            &details,
            Duration::from_millis(20),
            tokio::time::sleep(Duration::from_millis(80)),
        )
        .await;
        assert!(seen.lock().unwrap().is_empty());
    }
}
