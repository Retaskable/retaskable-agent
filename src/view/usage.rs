//! The snapshot's `usage` windows (agent/09.2-what-the-computer-reports.md, "Usage"; `usageView` in the TypeScript
//! agent's `tiers.ts`): the 5-hour and weekly readings the loop keeps in the project's `state.json`, and the tiers
//! they pick with the computer's `models` config.
use std::path::Path;

use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::AgentView;
use crate::paths;
use crate::store::Store;
use crate::tiers;

/// `epoch` seconds as ISO 8601, null once it is not after `now`.
fn iso_if_future(epoch: Option<i64>, now: i64) -> Value {
    epoch
        .filter(|e| *e > now)
        .and_then(|e| OffsetDateTime::from_unix_timestamp(e).ok())
        .and_then(|t| t.format(&Rfc3339).ok())
        .map_or(Value::Null, Value::from)
}

/// The 5-hour window of a reading: `unifiedWindows.five_hour`, else the reading itself unless it is labeled with
/// another window.
fn five_hour(usage: &Value) -> Option<&Value> {
    if let Some(w) = usage.get("unifiedWindows").and_then(|w| w.get("five_hour")) {
        return Some(w);
    }
    match usage.get("rateLimitType").and_then(Value::as_str) {
        Some("five_hour") | None => Some(usage),
        Some(_) => None,
    }
}

/// The window fields, tiers and flags of `usage` at `now` (epoch seconds).
pub(super) fn usage_view(five: Option<&Value>, week: Option<&Value>, now: i64, local_up: Option<bool>) -> Value {
    let window = five.and_then(five_hour);
    let resets_at = window.and_then(|w| w.get("resetsAt")).and_then(Value::as_i64);
    let fresh = resets_at.is_some_and(|r| r > now);
    let left = window
        .filter(|_| fresh)
        .and_then(|w| w.get("utilization"))
        .and_then(Value::as_f64)
        .map(|u| ((1.0 - tiers::frac(u)).max(0.0) * 100.0).round());
    let status = window
        .filter(|_| fresh)
        .and_then(|w| w.get("status"))
        .and_then(Value::as_str)
        .or_else(|| {
            // The older shape keeps `status` at the top while the window is the reading itself, so both come to the same.
            five.filter(|_| fresh)?.get("status")?.as_str()
        });
    let week_reading = [week, five].into_iter().flatten().find_map(tiers::week_reading);
    let (week_used, week_resets) = week_reading.unwrap_or((None, None));
    let week_live = week_resets.is_some_and(|at| at > now);
    json!({
        "leftPct": left,
        "status": status,
        "resetsAt": iso_if_future(resets_at, now),
        "weekUsedPct": week_used.filter(|_| week_live).map(|u| (u * 100.0).round()),
        "weekResetsAt": iso_if_future(week_resets, now),
        "localUp": local_up,
        "subscription": tiers::on_subscription(five, now),
    })
}

impl AgentView {
    /// The `usage` windows for `project`, when `snapshot-reporting` is On and the project is set up.
    pub(super) fn usage_block(&self, project: &str, data_dir: &Path) -> Option<Value> {
        if !self.settings.snapshot_reporting_on() || !self.projects.iter().any(|p| p.id == project) {
            return None;
        }
        let store = Store::load(&paths::project_dir(data_dir, project)).ok();
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let local_up = self.local.enabled().then(|| self.local.available());
        Some(usage_view(
            store.as_ref().and_then(Store::usage).as_ref(),
            store.as_ref().and_then(Store::week_usage).as_ref(),
            now,
            local_up,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    fn view(five: Option<Value>, week: Option<Value>, local_up: Option<bool>) -> Value {
        usage_view(five.as_ref(), week.as_ref(), NOW, local_up)
    }

    fn five(left_used: f64, resets: i64) -> Value {
        json!({"unifiedWindows": {"five_hour": {"utilization": left_used, "resetsAt": resets, "status": "allowed"}}})
    }

    fn week(used: f64, resets: i64) -> Value {
        json!({"rateLimitType": "seven_day", "utilization": used, "resetsAt": resets})
    }

    #[test]
    fn both_windows_give_percentages_resets_and_tiers() {
        let v = view(Some(five(0.25, NOW + 3600)), Some(week(0.4, NOW + 86_400)), None);
        assert_eq!(v["leftPct"], 75.0);
        assert_eq!(v["status"], "allowed");
        assert_eq!(v["resetsAt"], "2023-11-14T23:13:20Z");
        assert_eq!(v["weekUsedPct"], 40.0);
        assert_eq!(v["weekResetsAt"], "2023-11-15T22:13:20Z");
        assert_eq!(v["localUp"], Value::Null);
        assert_eq!(v["subscription"], true);
    }

    #[test]
    fn a_reading_past_its_reset_has_null_resets() {
        let v = view(Some(five(0.9, NOW - 10)), Some(week(0.4, NOW - 10)), None);
        assert_eq!(v["resetsAt"], Value::Null);
        assert_eq!(v["weekResetsAt"], Value::Null);
        assert_eq!(v["leftPct"], Value::Null);
        assert_eq!(v["weekUsedPct"], Value::Null);
    }

    #[test]
    fn a_week_reading_with_no_reset_time_has_no_used_share() {
        let v = view(
            None,
            Some(json!({"rateLimitType": "seven_day", "utilization": 0.9})),
            None,
        );
        assert_eq!(v["weekUsedPct"], Value::Null);
        assert_eq!(v["weekResetsAt"], Value::Null);
    }

    #[test]
    fn no_reading_gives_nulls() {
        let v = view(None, None, None);
        for key in [
            "leftPct",
            "status",
            "resetsAt",
            "weekUsedPct",
            "weekResetsAt",
            "localUp",
        ] {
            assert_eq!(v[key], Value::Null, "{key}");
        }
        assert_eq!(v["subscription"], false);
    }

    #[test]
    fn a_lone_weekly_reading_and_the_local_model_show() {
        let v = view(Some(week(0.5, NOW + 100)), None, Some(true));
        assert_eq!(v["weekUsedPct"], 50.0);
        assert_eq!(v["leftPct"], Value::Null);
        assert_eq!(v["localUp"], true);
        assert_eq!(view(None, None, Some(false))["localUp"], false);
    }
}
