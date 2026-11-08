//! The snapshot's `usage` windows (agent/09.2-what-the-computer-reports.md, `usageView` in `agent/src/tiers.ts`):
//! the 5-hour window's left share and status, the week's used share, both resets, the three tiers, whether the
//! local model is up and whether this computer is on a subscription.
use std::path::Path;

use serde_json::{Value, json};

use super::AgentView;
use crate::config::ModelsConfig;
use crate::orchestrator::live::iso8601_at;
use crate::paths;
use crate::store::Store;
use crate::tiers::{self, LOCAL, OPUS, SONNET};

impl AgentView {
    /// The usage fields for `project` while `snapshot-reporting` is On, read from the readings its loop keeps in
    /// `state.json`; none with the flag Off or before Setup.
    pub(super) fn usage_json(&self, project: &str, data_dir: &Path) -> Option<Value> {
        if !self.settings.snapshot_reporting_on() {
            return None;
        }
        self.projects.iter().find(|p| p.id == project)?;
        let store = Store::load(&paths::project_dir(data_dir, project)).ok();
        let local_up = self.local.enabled().then(|| self.local.available());
        Some(view(
            store.as_ref().and_then(Store::usage).as_ref(),
            store.as_ref().and_then(Store::week_usage).as_ref(),
            &self.models,
            local_up,
            time::OffsetDateTime::now_utc().unix_timestamp(),
        ))
    }
}

/// `epoch` as ISO 8601 when it is still ahead of `now`.
fn future_iso(epoch: Option<i64>, now: i64) -> Option<String> {
    epoch.filter(|&at| at > now).map(iso8601_at)
}

/// The 5-hour window's `(utilization, resetsAt)`: `unifiedWindows.five_hour`, else the flat reading.
fn five_hour(usage: &Value) -> (Option<f64>, Option<i64>) {
    let window = usage.get("unifiedWindows").and_then(|w| w.get("five_hour"));
    let flat = window.is_none() && tiers::five_hour_resets_at(usage).is_some();
    let w = window.or(flat.then_some(usage));
    (
        w.and_then(|w| w.get("utilization"))
            .and_then(Value::as_f64)
            .map(tiers::frac),
        tiers::five_hour_resets_at(usage),
    )
}

/// The `usage` fields from the latest 5-hour reading and the latest weekly one (null for what a reading doesn't
/// carry or whose reset has passed).
fn view(usage: Option<&Value>, week: Option<&Value>, models: &ModelsConfig, local_up: Option<bool>, now: i64) -> Value {
    let (utilization, resets_at) = usage.map(five_hour).unwrap_or_default();
    let live = resets_at.is_some_and(|at| at > now);
    let left_pct = utilization
        .filter(|_| live)
        .map(|u| ((1.0 - u) * 100.0).max(0.0).round() as i64);
    let status = match usage {
        Some(u) if live => u.get("status").and_then(Value::as_str).unwrap_or("unknown"),
        Some(_) => "allowed",
        None => "unknown",
    };
    let (week_used, week_resets) = week
        .and_then(tiers::week_reading)
        .or_else(|| usage.and_then(tiers::week_reading))
        .unwrap_or_default();
    let week_live = week_resets.is_some_and(|at| at > now);
    let up = local_up.unwrap_or(false);
    let cut_off = tiers::week_cutoff_until(week.or(usage), now, models).is_some();
    let (tier, scout, top) = if cut_off {
        if up {
            (LOCAL, LOCAL, LOCAL)
        } else {
            (SONNET, SONNET, OPUS)
        }
    } else {
        (
            tiers::implementer_tier(usage, now, models, up),
            tiers::scout_tier(usage, now, models, up),
            tiers::top_tier(usage, now, models, up),
        )
    };
    json!({
        "leftPct": left_pct,
        "status": status,
        "resetsAt": future_iso(resets_at, now),
        "weekUsedPct": week_used.filter(|_| week_live).map(|u| (u * 100.0).round() as i64),
        "weekResetsAt": future_iso(week_resets.filter(|_| week_live), now),
        "tier": tier,
        "scoutTier": scout,
        "topTier": top,
        "localUp": local_up,
        "subscription": tiers::on_subscription(usage, now),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    fn five(left_used: f64, resets: i64) -> Value {
        json!({ "status": "allowed", "unifiedWindows": { "five_hour": { "utilization": left_used, "resetsAt": resets } } })
    }

    fn week(used: f64, resets: i64) -> Value {
        json!({ "status": "allowed", "unifiedWindows": { "seven_day": { "utilization": used, "resetsAt": resets } } })
    }

    #[test]
    fn a_five_hour_and_a_weekly_reading_fill_every_field() {
        let v = view(
            Some(&five(0.7, NOW + 3600)),
            Some(&week(0.42, NOW + 86_400)),
            &ModelsConfig::default(),
            Some(false),
            NOW,
        );
        assert_eq!(v["leftPct"], 30);
        assert_eq!(v["status"], "allowed");
        assert_eq!(v["resetsAt"], iso8601_at(NOW + 3600));
        assert_eq!(v["weekUsedPct"], 42);
        assert_eq!(v["weekResetsAt"], iso8601_at(NOW + 86_400));
        assert_eq!(v["localUp"], false);
        assert_eq!(v["subscription"], true);
        assert_eq!(v["tier"], SONNET);
        assert_eq!(v["scoutTier"], SONNET);
        assert_eq!(v["topTier"], OPUS);
    }

    #[test]
    fn a_reading_past_its_reset_shows_null_resets() {
        let v = view(
            Some(&five(0.9, NOW - 10)),
            Some(&week(0.5, NOW - 10)),
            &ModelsConfig::default(),
            None,
            NOW,
        );
        assert_eq!(v["leftPct"], Value::Null);
        assert_eq!(v["resetsAt"], Value::Null);
        assert_eq!(v["weekUsedPct"], Value::Null);
        assert_eq!(v["weekResetsAt"], Value::Null);
        assert_eq!(v["status"], "allowed");
        assert_eq!(v["tier"], OPUS);
    }

    #[test]
    fn no_reading_gives_nulls() {
        let v = view(None, None, &ModelsConfig::default(), None, NOW);
        assert_eq!(v["leftPct"], Value::Null);
        assert_eq!(v["status"], "unknown");
        assert_eq!(v["resetsAt"], Value::Null);
        assert_eq!(v["weekUsedPct"], Value::Null);
        assert_eq!(v["weekResetsAt"], Value::Null);
        assert_eq!(v["localUp"], Value::Null);
        assert_eq!(v["subscription"], false);
        assert_eq!(
            (v["tier"].clone(), v["scoutTier"].clone(), v["topTier"].clone()),
            (json!(OPUS), json!(SONNET), json!(OPUS))
        );
    }

    #[test]
    fn a_low_window_with_the_local_model_up_moves_the_tiers_to_local() {
        let v = view(
            Some(&five(0.97, NOW + 60)),
            None,
            &ModelsConfig::default(),
            Some(true),
            NOW,
        );
        assert_eq!(v["leftPct"], 3);
        assert_eq!(v["tier"], LOCAL);
        assert_eq!(v["scoutTier"], LOCAL);
    }

    #[test]
    fn a_lone_weekly_reading_reports_the_week_only() {
        let v = view(None, Some(&week(0.5, NOW + 100)), &ModelsConfig::default(), None, NOW);
        assert_eq!(v["leftPct"], Value::Null);
        assert_eq!(v["weekUsedPct"], 50);
        assert_eq!(v["weekResetsAt"], iso8601_at(NOW + 100));
    }
}
