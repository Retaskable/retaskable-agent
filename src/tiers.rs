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
    if let Some(window) = usage.get("unifiedWindows").and_then(|w| w.get("five_hour")) {
        return tier_from_five_hour_window(window, now_epoch, models);
    }
    if let Some(pct_left) = top_level_pct_left(usage, now_epoch) {
        return tier_from_pct_left(pct_left, models);
    }
    match fresh_status(usage, now_epoch) {
        Some("allowed_warning") | Some("rejected") => SONNET,
        _ => OPUS,
    }
}

/// Whether a `rate_limit_info` reading is (or carries) the 5-hour window `implementer_tier` reads,
/// rather than some other window a build sent alone (`parseRateLimit` in the TypeScript agent):
/// most builds report one window per event with no `rateLimitType` at all, a build that labels its
/// window keeps only `"five_hour"`, and a reading with `unifiedWindows.five_hour` always counts,
/// whatever its own `rateLimitType` says. Anything else — an older build's lone `seven_day` event,
/// say — would otherwise overwrite a usable 5-hour reading with one this agent can't read a tier
/// from, each time Claude Code happens to report it.
///
/// Not yet: the weekly limit itself (a `rateLimitType: "seven_day"` rejection, which the TypeScript
/// agent keeps instead of dropping) isn't read for anything here, so such a reading is dropped
/// rather than kept for its own sake.
pub fn is_five_hour_reading(usage: &Value) -> bool {
    if usage.get("unifiedWindows").and_then(|w| w.get("five_hour")).is_some() {
        return true;
    }
    match usage.get("rateLimitType").and_then(Value::as_str) {
        Some(rate_limit_type) => rate_limit_type == "five_hour",
        None => true,
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

/// Decides straight from `rate_limit_info.unifiedWindows.five_hour` (newer Claude Code builds):
/// its own `resetsAt`, at or before `now`, makes it a full window (`opus`) on its own — the
/// top-level `status` is never consulted once this window exists, since it can belong to a later,
/// unrelated reading (`is_five_hour_reading`, settled in `src/orchestrator/driver/settle.rs`).
fn tier_from_five_hour_window(window: &Value, now_epoch: i64, models: &ModelsConfig) -> &'static str {
    if let Some(resets_at) = window.get("resetsAt").and_then(Value::as_i64)
        && resets_at <= now_epoch
    {
        return OPUS;
    }
    match window.get("utilization").and_then(Value::as_f64) {
        Some(utilization) => tier_from_pct_left(pct_left(utilization), models),
        None => OPUS,
    }
}

/// The older, flat `rate_limit_info.utilization` (`parseRateLimit`'s `frac(info.utilization)` in
/// the TypeScript agent), read against the top-level `resetsAt`, when there's no
/// `unifiedWindows.five_hour` to read instead. `None` when there's no fresh percentage to read —
/// `implementer_tier` then falls back to `fresh_status`.
fn top_level_pct_left(usage: &Value, now_epoch: i64) -> Option<f64> {
    let resets_at = usage.get("resetsAt")?.as_i64()?;
    if resets_at <= now_epoch {
        return None;
    }
    let utilization = usage.get("utilization")?.as_f64()?;
    Some(pct_left(utilization))
}

/// The old-format `rate_limit_info.status`, when its own `resetsAt` hasn't passed yet.
fn fresh_status(usage: &Value, now_epoch: i64) -> Option<&str> {
    let resets_at = usage.get("resetsAt")?.as_i64()?;
    if resets_at <= now_epoch {
        return None;
    }
    usage.get("status")?.as_str()
}

fn tier_from_pct_left(pct_left: f64, models: &ModelsConfig) -> &'static str {
    if pct_left >= f64::from(models.opus_above_pct) {
        OPUS
    } else {
        SONNET
    }
}

/// A usage fraction, or a whole-number percentage above 1 (`frac` in the TypeScript agent's
/// `parseRateLimit`) — Claude Code sends either shape for the same field.
fn frac(value: f64) -> f64 {
    if value > 1.0 { value / 100.0 } else { value }
}

/// The percentage of a 5-hour window still left, from its `utilization` (see `frac`).
fn pct_left(utilization: f64) -> f64 {
    (1.0 - frac(utilization)).max(0.0) * 100.0
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

    #[test]
    fn a_flat_top_level_utilization_with_no_unified_windows_is_read_too() {
        let usage = json!({"status": "allowed", "utilization": 0.9, "resetsAt": NOW + 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models()), SONNET);
    }

    #[test]
    fn a_utilization_given_as_a_whole_number_percentage_is_normalized_not_clamped() {
        // Clamped to 1.0 (fully used), this would wrongly read as sonnet; as 10% used it's opus.
        let usage = json!({"unifiedWindows": {"five_hour": {"utilization": 10, "resetsAt": NOW + 1}}});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models()), OPUS);
    }

    #[test]
    fn a_reset_five_hour_window_is_full_even_when_the_top_level_status_is_not() {
        let usage = json!({
            "status": "allowed_warning",
            "resetsAt": NOW + 1,
            "unifiedWindows": {"five_hour": {"utilization": 0.9, "resetsAt": NOW - 1}},
        });
        assert_eq!(implementer_tier(Some(&usage), NOW, &models()), OPUS);
    }

    #[test]
    fn a_reading_with_no_rate_limit_type_is_a_five_hour_reading() {
        assert!(is_five_hour_reading(&json!({"status": "allowed"})));
    }

    #[test]
    fn a_reading_labeled_five_hour_is_a_five_hour_reading() {
        assert!(is_five_hour_reading(
            &json!({"status": "allowed", "rateLimitType": "five_hour"})
        ));
    }

    #[test]
    fn a_reading_labeled_something_else_with_no_unified_five_hour_window_is_not() {
        assert!(!is_five_hour_reading(
            &json!({"status": "rejected", "rateLimitType": "seven_day"})
        ));
    }

    #[test]
    fn a_reading_with_a_unified_five_hour_window_counts_whatever_its_own_type_says() {
        let usage = json!({
            "rateLimitType": "seven_day",
            "unifiedWindows": {"five_hour": {"utilization": 0.5, "resetsAt": NOW + 1}},
        });
        assert!(is_five_hour_reading(&usage));
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
                notes: Some("n".into()),
                at: Some("2026-10-29T00:00:00Z".into()),
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
