//! Picking the implementer's model from the 5-hour usage limit still left
//! (docs/specs/agent/07-orchestrator-loop.md section 7.1, `agent/src/tiers.ts` in the TypeScript
//! agent). Only the implementer's own opus/sonnet switch is built here: the other roles (scouts,
//! the drafter, the triager, the planner, the final reviewer) have no sessions in this agent yet,
//! and the local-model fallback and the wait-for-reset floor at `models.pauseBelowPct` aren't
//! wired to anything that can act on them (no session starts against a local server, and no
//! "waiting for the limit" state), so both run on `sonnet` for now, same as everything below
//! `opusAbovePct`.
use serde_json::Value;

use crate::backlog::Item;
use crate::config::ModelsConfig;

pub const OPUS: &str = "opus";
pub const SONNET: &str = "sonnet";

/// The model `ClaudeStarter` passes `--model` for a fresh implementer session, from the latest
/// 5-hour usage reading kept in `state.usage` (`None` when there's none yet, which "is treated as
/// full", i.e. `opus`).
pub fn implementer_tier(usage: Option<&Value>, now_epoch: i64, models: &ModelsConfig) -> &'static str {
    let Some(usage) = usage else { return OPUS };
    if let Some(pct_left) = five_hour_pct_left(usage, now_epoch) {
        return if pct_left >= f64::from(models.opus_above_pct) {
            OPUS
        } else {
            SONNET
        };
    }
    match fresh_status(usage, now_epoch) {
        Some("allowed_warning") | Some("rejected") => SONNET,
        _ => OPUS,
    }
}

/// `implementerModel` (agent/src/orchestratorHelpers.ts in the TypeScript agent): where
/// `implementer_tier` gives `opus`, a roadmap item's first try instead runs on `sonnet`, so Opus is
/// kept for retries, reopened items and the bigger ones. Only a tier of `opus` is ever moved; a tier
/// already below it (from a usage reading, section 7.1's table) is returned unchanged.
///
/// Not yet: the store's claimed item (backlog/06.1-an-item.md) carries no retry count and no size
/// (estimates.md section 3.2 — sizing came from the TypeScript agent's own planner, removed before
/// this rule existed, so an item from the store never has one), so both are read as a first try on
/// a size below L until the store carries them; only `reopened`, which the store does send, can keep
/// `opus` here today.
pub fn implementer_model(tier: &'static str, item: &Item) -> &'static str {
    if tier != OPUS {
        return tier;
    }
    if item.reopened.is_some() { OPUS } else { SONNET }
}

/// `rate_limit_info.unifiedWindows.five_hour`'s percentage still left (newer Claude Code builds),
/// or `None` when that window is missing or has already reset (`resetsAt` at or before `now`).
fn five_hour_pct_left(usage: &Value, now_epoch: i64) -> Option<f64> {
    let window = usage.get("unifiedWindows")?.get("five_hour")?;
    let resets_at = window.get("resetsAt")?.as_i64()?;
    if resets_at <= now_epoch {
        return None;
    }
    let utilization = window.get("utilization")?.as_f64()?;
    Some((1.0 - utilization.clamp(0.0, 1.0)) * 100.0)
}

/// The old-format `rate_limit_info.status`, when its own `resetsAt` hasn't passed yet.
fn fresh_status(usage: &Value, now_epoch: i64) -> Option<&str> {
    let resets_at = usage.get("resetsAt")?.as_i64()?;
    if resets_at <= now_epoch {
        return None;
    }
    usage.get("status")?.as_str()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const NOW: i64 = 1_700_000_000;
    fn models() -> ModelsConfig {
        ModelsConfig::default()
    }

    #[test]
    fn no_reading_is_treated_as_a_full_window() {
        assert_eq!(implementer_tier(None, NOW, &models()), OPUS);
    }

    #[test]
    fn a_window_that_already_reset_is_treated_as_full() {
        let usage = json!({"status": "rejected", "resetsAt": NOW - 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models()), OPUS);
    }

    #[test]
    fn five_hour_utilization_at_or_above_opus_above_pct_left_is_opus() {
        let usage = json!({"unifiedWindows": {"five_hour": {"utilization": 0.3, "resetsAt": NOW + 1}}});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models()), OPUS);
    }

    #[test]
    fn five_hour_utilization_below_opus_above_pct_left_is_sonnet() {
        let usage = json!({"unifiedWindows": {"five_hour": {"utilization": 0.85, "resetsAt": NOW + 1}}});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models()), SONNET);
    }

    #[test]
    fn a_custom_opus_above_pct_moves_the_boundary() {
        let usage = json!({"unifiedWindows": {"five_hour": {"utilization": 0.3, "resetsAt": NOW + 1}}});
        let mut models = models();
        models.opus_above_pct = 80;
        assert_eq!(implementer_tier(Some(&usage), NOW, &models), SONNET);
    }

    #[test]
    fn status_allowed_is_opus() {
        let usage = json!({"status": "allowed", "resetsAt": NOW + 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models()), OPUS);
    }

    #[test]
    fn status_allowed_warning_is_sonnet() {
        let usage = json!({"status": "allowed_warning", "resetsAt": NOW + 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models()), SONNET);
    }

    #[test]
    fn status_rejected_is_sonnet_for_now_rather_than_waiting() {
        let usage = json!({"status": "rejected", "resetsAt": NOW + 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models()), SONNET);
    }

    #[test]
    fn an_unrecognized_status_is_treated_as_full() {
        let usage = json!({"status": "something_new", "resetsAt": NOW + 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models()), OPUS);
    }

    fn item(reopened: bool) -> Item {
        Item {
            id: "x".into(),
            title: "x".into(),
            status: crate::backlog::ItemStatus::Todo,
            rank: 1,
            do_next: false,
            depends_on: Vec::new(),
            spec_refs: Vec::new(),
            slice: 0,
            remaining: None,
            pr_url: None,
            branch: None,
            kind: None,
            milestone: None,
            reopened: reopened.then(|| crate::backlog::Reopened {
                notes: "n".into(),
                at: "2026-10-29T00:00:00Z".into(),
                by: None,
            }),
        }
    }

    #[test]
    fn a_first_try_at_a_full_window_runs_on_sonnet_not_opus() {
        assert_eq!(implementer_model(OPUS, &item(false)), SONNET);
    }

    #[test]
    fn a_reopened_item_keeps_opus() {
        assert_eq!(implementer_model(OPUS, &item(true)), OPUS);
    }

    #[test]
    fn a_tier_already_below_opus_is_kept_whatever_the_item_is() {
        assert_eq!(implementer_model(SONNET, &item(true)), SONNET);
    }
}
