//! The snapshot's `usage.cursor` (agent/09.2-what-the-computer-reports.md): Cursor's tokens, with `snapshot-reporting` On.
use serde_json::{Value, json};

use super::AgentView;
use crate::store::MachineStore;

impl AgentView {
    /// `{ todayTokens, weekInputTokens, weekOutputTokens, limitHitAt? }` from this computer's machine file; `None`
    /// until a Cursor session has reported tokens.
    pub(super) fn cursor_usage(&self) -> Option<Value> {
        let machine = MachineStore::load(self.settings.dir()?).ok()?;
        let today = machine.cursor_today_tokens()?;
        let (input, output) = machine.cursor_week_tokens()?;
        let mut cursor = json!({ "todayTokens": today, "weekInputTokens": input, "weekOutputTokens": output });
        if let Some(at) = machine
            .cursor_limit_at()
            .and_then(chrono::DateTime::from_timestamp_millis)
        {
            cursor["limitHitAt"] = json!(at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        }
        Some(cursor)
    }
}
