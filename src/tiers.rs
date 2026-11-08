//! Picking the implementer's model from the 5-hour usage limit still left
//! (docs/specs/agent/07-orchestrator-loop.md section 7.1, `agent/src/tiers.ts` in the TypeScript
//! agent). The implementer, scout, mid and top tiers are built here; the local model is chosen
//! only where `local_available` is true, and no caller passes that yet. Below `pauseBelowPct`
//! (or `rejected`) the implementer and scouts stay on `sonnet` without it, because no
//! "waiting for the limit" state exists in this agent yet.
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::Value;

use crate::backlog::Item;
use crate::config::ModelsConfig;

pub const OPUS: &str = "opus";
pub const SONNET: &str = "sonnet";
pub const LOCAL: &str = "local";

/// Which row of section 7.1's tables a usage reading falls on.
#[derive(Clone, Copy, PartialEq)]
enum Band {
    /// At or above `opusAbovePct` left (also no reading, and `allowed`).
    Full,
    /// At or above `sonnetAbovePct`.
    Mid,
    /// At or above `pauseBelowPct`.
    Low,
    /// Below `pauseBelowPct`, or `rejected`.
    Floor,
    /// `allowed_warning` with a status and no percentage.
    Warning,
}

fn band(usage: Option<&Value>, now_epoch: i64, models: &ModelsConfig) -> Band {
    let Some(usage) = usage else { return Band::Full };
    if let Some(window) = usage.get("unifiedWindows").and_then(|w| w.get("five_hour")) {
        return band_from_five_hour_window(window, now_epoch, models);
    }
    if let Some(pct_left) = top_level_pct_left(usage, now_epoch) {
        return band_from_pct_left(pct_left, models);
    }
    match fresh_status(usage, now_epoch) {
        Some("allowed_warning") => Band::Warning,
        Some("rejected") => Band::Floor,
        _ => Band::Full,
    }
}

/// `local` when the local model is available, else `fallback`.
fn local_or(local_available: bool, fallback: &'static str) -> &'static str {
    if local_available { LOCAL } else { fallback }
}

/// The model `ClaudeStarter` passes `--model` for a fresh implementer session, from the latest
/// 5-hour usage reading kept in `state.usage` (`None` when there's none yet, which "is treated as
/// full", i.e. `opus`). Below `sonnetAbovePct` it is `local` when `local_available`, else `sonnet`.
pub fn implementer_tier(
    usage: Option<&Value>,
    now_epoch: i64,
    models: &ModelsConfig,
    local_available: bool,
) -> &'static str {
    match band(usage, now_epoch, models) {
        Band::Full => OPUS,
        Band::Mid | Band::Warning => SONNET,
        Band::Low | Band::Floor => local_or(local_available, SONNET),
    }
}

/// `scoutTier` (section 7.1): never Opus; `local` (else `sonnet`) below `sonnetAbovePct` and on `allowed_warning`.
pub fn scout_tier(usage: Option<&Value>, now_epoch: i64, models: &ModelsConfig, local_available: bool) -> &'static str {
    match band(usage, now_epoch, models) {
        Band::Full | Band::Mid => SONNET,
        Band::Low | Band::Floor | Band::Warning => local_or(local_available, SONNET),
    }
}

/// Whether a `rate_limit_info` reading is one to keep as the usage reading (`parseRateLimit` in the TypeScript
/// agent): the 5-hour window `implementer_tier` reads, or, with `weekly` (the `weekly-limit-reading` flag), a
/// weekly rejection. Most builds report one window per event with no `rateLimitType` at all, a build that labels
/// its window keeps `"five_hour"`, and a reading with `unifiedWindows.five_hour` always counts, whatever its own
/// `rateLimitType` says. With `weekly`, a reading labeled `"seven_day"` whose `status` is `rejected` counts too:
/// it is what turned the session away, and its reset is the week's. Anything else — an older build's lone
/// non-rejected `seven_day` event, say — would otherwise overwrite a usable 5-hour reading with one this agent
/// can't read a tier from, each time Claude Code happens to report it.
pub fn is_usage_reading(usage: &Value, weekly: bool) -> bool {
    if usage.get("unifiedWindows").and_then(|w| w.get("five_hour")).is_some() {
        return true;
    }
    match usage.get("rateLimitType").and_then(Value::as_str) {
        Some("seven_day") => weekly && usage["status"] == "rejected",
        Some(rate_limit_type) => rate_limit_type == "five_hour",
        None => true,
    }
}

/// Whether the `weekly-limit-reading` or `weekly-cutoff` flag was On at its last answer; Off until the flags
/// client is handed over.
pub fn weekly_reading_on() -> bool {
    FLAG.get()
        .is_some_and(|f| f.last("weekly-limit-reading") || f.last("weekly-cutoff"))
}

/// Whether a window's `utilization` is full, read like `frac` (a whole number over 1 is a percentage).
pub fn utilization_full(utilization: f64) -> bool {
    frac(utilization) >= 1.0
}

/// A five-hour reading counts as "on a subscription" for this long after its window's reset
/// (`onSubscription` in `agent/src/tiers.ts`).
const SUBSCRIPTION_WEEK_SECS: i64 = 7 * 24 * 60 * 60;

/// Whether Claude Code has reported a five-hour usage window in the last week, i.e. this computer is on a Claude
/// subscription rather than an API key (`onSubscription`, docs/specs/engines/03-choosing-the-engine.md section 3.4).
/// False with no reading, a reading that isn't a five-hour one, or one whose window reset over a week ago.
pub fn on_subscription(usage: Option<&Value>, now_epoch: i64) -> bool {
    usage
        .and_then(five_hour_resets_at)
        .is_some_and(|resets_at| resets_at >= now_epoch - SUBSCRIPTION_WEEK_SECS)
}

/// The `resetsAt` of a five-hour reading: its `unifiedWindows.five_hour`'s, else the top-level one.
fn five_hour_resets_at(usage: &Value) -> Option<i64> {
    if let Some(window) = usage.get("unifiedWindows").and_then(|w| w.get("five_hour")) {
        return window.get("resetsAt").and_then(Value::as_i64);
    }
    if usage.get("rateLimitType").and_then(Value::as_str) != Some("five_hour") && usage.get("rateLimitType").is_some() {
        return None;
    }
    usage.get("resetsAt").and_then(Value::as_i64)
}

/// The computer's latest five-hour reset across every project's saved usage (0 when none). One signal for the whole
/// machine: a project's reading counts for every session (`subscription-zero-budget`).
static LATEST_FIVE_HOUR_RESET: AtomicI64 = AtomicI64::new(0);
static FLAG: std::sync::OnceLock<std::sync::Arc<crate::flags::Flags>> = std::sync::OnceLock::new();

/// Notes a project's saved usage reading in the machine-wide signal.
pub fn note_usage(usage: &Value) {
    if let Some(resets_at) = five_hour_resets_at(usage) {
        LATEST_FIVE_HOUR_RESET.fetch_max(resets_at, Ordering::Relaxed);
    }
}

/// Hands over the Retaskable flags client the `subscription-zero-budget` flag is read from.
pub fn set_flags(flags: std::sync::Arc<crate::flags::Flags>) {
    let _ = FLAG.set(flags);
}

/// Whether the `local-tier-ladder` flag was On at its last answer; Off until the flags client is handed over.
pub fn local_ladder_on() -> bool {
    FLAG.get().is_some_and(|f| f.last("local-tier-ladder"))
}

/// The model name `--model` gets for a Claude session on `tier`: `sonnet` for `local`, which the local session's
/// environment maps onto the local model (section 7.2).
pub fn claude_model(tier: &'static str) -> &'static str {
    if tier == LOCAL { SONNET } else { tier }
}

/// The model name a session on `engine` gets for `tier`: [`claude_model`] on Claude, the tier as it is elsewhere.
pub fn claude_model_for(engine: crate::engines::engine::EngineId, tier: &str) -> String {
    if engine == crate::engines::engine::EngineId::Claude && tier == LOCAL {
        SONNET
    } else {
        tier
    }
    .to_string()
}

/// Whether the `model-concurrency-caps` flag was On at its last answer; Off until the flags client is handed over.
pub fn model_caps_on() -> bool {
    FLAG.get().is_some_and(|f| f.last("model-concurrency-caps"))
}

/// Whether sessions run with no dollar cap right now: the flag is On and the computer is on a subscription.
pub fn machine_on_subscription(now_epoch: i64) -> bool {
    FLAG.get().is_some_and(|f| f.last("subscription-zero-budget")) && signal_on(now_epoch)
}

/// The machine-wide signal alone: some project has saved a five-hour reading from the last week.
pub fn signal_on(now_epoch: i64) -> bool {
    let resets_at = LATEST_FIVE_HOUR_RESET.load(Ordering::Relaxed);
    on_subscription(Some(&serde_json::json!({"resetsAt": resets_at})), now_epoch)
}

/// `midTier` (agent/src/tiers.ts, section 7.1): the implementer's `tier`, but never Opus. The drafter and the triager
/// write and read; Opus is kept for the planner and the final review.
pub fn mid_tier(tier: &'static str) -> &'static str {
    if tier == OPUS { SONNET } else { tier }
}

/// The model for chores and read-only sessions that don't need the top tier.
pub fn chore_tier() -> &'static str {
    SONNET
}

/// `topTier` (section 7.1): the planner, the final reviewer and the triager's second look stay on Opus
/// down to `pauseBelowPct`; below it (or `rejected`) they run `local` when available, else stay on Opus.
pub fn top_tier(usage: Option<&Value>, now_epoch: i64, models: &ModelsConfig, local_available: bool) -> &'static str {
    match band(usage, now_epoch, models) {
        Band::Floor => local_or(local_available, OPUS),
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

/// Decides straight from `rate_limit_info.unifiedWindows.five_hour` (newer Claude Code builds):
/// its own `resetsAt`, missing or at or before `now`, makes it a full window (`opus`) on its own —
/// the same as a missing `resetsAt` already does for `top_level_pct_left` and `fresh_status` below.
/// The top-level `status` is never consulted once this window exists, since it can belong to a
/// later, unrelated reading (`is_usage_reading`, settled in `src/orchestrator/driver/settle.rs`).
fn band_from_five_hour_window(window: &Value, now_epoch: i64, models: &ModelsConfig) -> Band {
    match window.get("resetsAt").and_then(Value::as_i64) {
        Some(resets_at) if resets_at > now_epoch => {}
        _ => return Band::Full,
    }
    match window.get("utilization").and_then(Value::as_f64) {
        Some(utilization) => band_from_pct_left(pct_left(utilization), models),
        None => Band::Full,
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

fn band_from_pct_left(pct_left: f64, models: &ModelsConfig) -> Band {
    if pct_left >= f64::from(models.opus_above_pct) {
        Band::Full
    } else if pct_left >= f64::from(models.sonnet_above_pct) {
        Band::Mid
    } else if pct_left >= f64::from(models.pause_below_pct) {
        Band::Low
    } else {
        Band::Floor
    }
}

/// A usage fraction, or a whole-number percentage above 1 (`frac` in the TypeScript agent's
/// `parseRateLimit`) — Claude Code sends either shape for the same field.
fn frac(value: f64) -> f64 {
    if value > 1.0 { value / 100.0 } else { value }
}

/// The week's `resetsAt` (epoch seconds) when `usage` says the weekly limit is cut off now: the week's used share
/// (`unifiedWindows.seven_day.utilization`, read like `frac`) at or past `models.week_cutoff_pct`, or that window
/// `rejected`; failing a `seven_day` window, a lone reading whose `rateLimitType` is `seven_day`, read the same way
/// from its top-level fields. `None` with no reading, a reset that is missing or not after `now_epoch`, or no
/// weekly figure.
pub fn week_cutoff_until(usage: Option<&Value>, now_epoch: i64, models: &ModelsConfig) -> Option<i64> {
    let usage = usage?;
    let window = match usage.get("unifiedWindows").and_then(|w| w.get("seven_day")) {
        Some(w) => w,
        None if usage.get("rateLimitType").and_then(Value::as_str) == Some("seven_day") => usage,
        None => return None,
    };
    let used = window
        .get("utilization")
        .and_then(Value::as_f64)
        .map(|u| frac(u) * 100.0);
    let cut = window.get("status").and_then(Value::as_str) == Some("rejected")
        || used.is_some_and(|u| u >= f64::from(models.week_cutoff_pct));
    let resets_at = window.get("resetsAt").and_then(Value::as_i64)?;
    (cut && resets_at > now_epoch).then_some(resets_at)
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

    #[test]
    fn week_cutoff_reads_the_weekly_percentage_and_reset() {
        let m = ModelsConfig::default();
        let until = |u: &Value| week_cutoff_until(Some(u), NOW, &m);
        let week = |u: Value, r: Value| json!({"unifiedWindows": {"seven_day": {"utilization": u, "resetsAt": r}}});
        assert_eq!(week_cutoff_until(None, NOW, &m), None);
        assert_eq!(until(&week(json!(0.95), json!(NOW + 5))), Some(NOW + 5));
        assert_eq!(until(&week(json!(0.96), json!(NOW + 5))), Some(NOW + 5));
        assert_eq!(until(&week(json!(0.94), json!(NOW + 5))), None);
        assert_eq!(until(&week(json!(96), json!(NOW + 5))), Some(NOW + 5));
        assert_eq!(until(&week(json!(94), json!(NOW + 5))), None);
        assert_eq!(until(&week(json!(0.99), json!(NOW - 1))), None);
        assert_eq!(until(&week(json!(0.99), json!(NOW))), None);
        assert_eq!(
            until(&json!({"unifiedWindows": {"seven_day": {"utilization": 0.99}}})),
            None
        );
        let rejected = json!({"unifiedWindows": {"seven_day": {"status": "rejected", "resetsAt": NOW + 7}}});
        assert_eq!(until(&rejected), Some(NOW + 7));
        let lone = json!({"rateLimitType": "seven_day", "status": "rejected", "resetsAt": NOW + 9});
        assert_eq!(until(&lone), Some(NOW + 9));
        let lone_pct = json!({"rateLimitType": "seven_day", "utilization": 0.96, "resetsAt": NOW + 9});
        assert_eq!(until(&lone_pct), Some(NOW + 9));
        let five =
            json!({"rateLimitType": "five_hour", "utilization": 0.99, "status": "rejected", "resetsAt": NOW + 9});
        assert_eq!(until(&five), None);
        let five_only = json!({"unifiedWindows": {"five_hour": {"utilization": 0.99, "resetsAt": NOW + 9}}});
        assert_eq!(until(&five_only), None);
        assert_eq!(until(&json!({"status": "allowed"})), None);
    }

    #[test]
    fn week_cutoff_honors_a_custom_percentage() {
        let m = ModelsConfig {
            week_cutoff_pct: 80,
            ..ModelsConfig::default()
        };
        let w = |u: f64| json!({"unifiedWindows": {"seven_day": {"utilization": u, "resetsAt": NOW + 5}}});
        assert_eq!(week_cutoff_until(Some(&w(0.8)), NOW, &m), Some(NOW + 5));
        assert_eq!(week_cutoff_until(Some(&w(0.79)), NOW, &m), None);
    }

    fn models() -> ModelsConfig {
        ModelsConfig::default()
    }

    #[test]
    fn on_subscription_reads_a_recent_five_hour_window() {
        let w = json!({"unifiedWindows": {"five_hour": {"utilization": 0.3, "resetsAt": NOW + 10}}});
        assert!(on_subscription(Some(&w), NOW));
        assert!(on_subscription(
            Some(&json!({"status": "allowed", "resetsAt": NOW - 3600})),
            NOW
        ));
    }

    #[test]
    fn on_subscription_is_false_without_a_recent_five_hour_reading() {
        assert!(!on_subscription(None, NOW));
        let seven = json!({"rateLimitType": "seven_day", "resetsAt": NOW + 10});
        assert!(!on_subscription(Some(&seven), NOW));
        let old = json!({"status": "allowed", "resetsAt": NOW - 8 * 24 * 3600});
        assert!(!on_subscription(Some(&old), NOW));
        let old_window = json!({"unifiedWindows": {"five_hour": {"resetsAt": NOW - 8 * 24 * 3600}}});
        assert!(!on_subscription(Some(&old_window), NOW));
    }

    #[test]
    fn no_reading_is_treated_as_a_full_window() {
        assert_eq!(implementer_tier(None, NOW, &models(), false), OPUS);
    }

    #[test]
    fn a_window_that_already_reset_is_treated_as_full() {
        let usage = json!({"status": "rejected", "resetsAt": NOW - 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), OPUS);
    }

    #[test]
    fn five_hour_utilization_at_or_above_opus_above_pct_left_is_opus() {
        let usage = json!({"unifiedWindows": {"five_hour": {"utilization": 0.3, "resetsAt": NOW + 1}}});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), OPUS);
    }

    #[test]
    fn five_hour_utilization_below_opus_above_pct_left_is_sonnet() {
        let usage = json!({"unifiedWindows": {"five_hour": {"utilization": 0.85, "resetsAt": NOW + 1}}});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), SONNET);
    }

    #[test]
    fn a_custom_opus_above_pct_moves_the_boundary() {
        let usage = json!({"unifiedWindows": {"five_hour": {"utilization": 0.3, "resetsAt": NOW + 1}}});
        let mut models = models();
        models.opus_above_pct = 80;
        assert_eq!(implementer_tier(Some(&usage), NOW, &models, false), SONNET);
    }

    #[test]
    fn status_allowed_is_opus() {
        let usage = json!({"status": "allowed", "resetsAt": NOW + 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), OPUS);
    }

    #[test]
    fn status_allowed_warning_is_sonnet() {
        let usage = json!({"status": "allowed_warning", "resetsAt": NOW + 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), SONNET);
    }

    #[test]
    fn status_rejected_is_sonnet_for_now_rather_than_waiting() {
        let usage = json!({"status": "rejected", "resetsAt": NOW + 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), SONNET);
    }

    #[test]
    fn an_unrecognized_status_is_treated_as_full() {
        let usage = json!({"status": "something_new", "resetsAt": NOW + 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), OPUS);
    }

    #[test]
    fn a_flat_top_level_utilization_with_no_unified_windows_is_read_too() {
        let usage = json!({"status": "allowed", "utilization": 0.9, "resetsAt": NOW + 1});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), SONNET);
    }

    #[test]
    fn a_utilization_given_as_a_whole_number_percentage_is_normalized_not_clamped() {
        // Clamped to 1.0 (fully used), this would wrongly read as sonnet; as 10% used it's opus.
        let usage = json!({"unifiedWindows": {"five_hour": {"utilization": 10, "resetsAt": NOW + 1}}});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), OPUS);
    }

    #[test]
    fn a_reset_five_hour_window_is_full_even_when_the_top_level_status_is_not() {
        let usage = json!({
            "status": "allowed_warning",
            "resetsAt": NOW + 1,
            "unifiedWindows": {"five_hour": {"utilization": 0.9, "resetsAt": NOW - 1}},
        });
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), OPUS);
    }

    #[test]
    fn a_five_hour_window_with_no_resets_at_is_treated_as_full() {
        let usage = json!({"unifiedWindows": {"five_hour": {"utilization": 0.9}}});
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), OPUS);
    }

    #[test]
    fn a_reading_with_no_rate_limit_type_is_a_five_hour_reading() {
        assert!(is_usage_reading(&json!({"status": "allowed"}), false));
    }

    #[test]
    fn a_reading_labeled_five_hour_is_a_five_hour_reading() {
        assert!(is_usage_reading(
            &json!({"status": "allowed", "rateLimitType": "five_hour"}),
            true
        ));
    }

    #[test]
    fn a_reading_labeled_something_else_with_no_unified_five_hour_window_is_not() {
        assert!(!is_usage_reading(
            &json!({"status": "allowed", "rateLimitType": "seven_day"}),
            true
        ));
        assert!(!is_usage_reading(
            &json!({"status": "rejected", "rateLimitType": "seven_day"}),
            false
        ));
        assert!(!is_usage_reading(
            &json!({"status": "rejected", "rateLimitType": "overage"}),
            true
        ));
    }

    #[test]
    fn a_lone_weekly_rejection_is_kept_with_the_flag_and_reads_as_sonnet() {
        let usage = json!({"status": "rejected", "rateLimitType": "seven_day", "resetsAt": NOW + 100});
        assert!(is_usage_reading(&usage, true));
        assert_eq!(implementer_tier(Some(&usage), NOW, &models(), false), SONNET);
    }

    #[test]
    fn a_reading_with_a_unified_five_hour_window_counts_whatever_its_own_type_says() {
        let usage = json!({
            "rateLimitType": "seven_day",
            "unifiedWindows": {"five_hour": {"utilization": 0.5, "resetsAt": NOW + 1}},
        });
        assert!(is_usage_reading(&usage, false));
    }

    fn windowed(utilization: f64) -> Value {
        json!({"unifiedWindows": {"five_hour": {"utilization": utilization, "resetsAt": NOW + 1}}})
    }

    #[test]
    fn every_row_of_the_percentage_table_for_every_tier() {
        // (pct left, implementer, scout, top) with local available / not available.
        let rows = [
            (0.4, OPUS, SONNET, OPUS),   // 60% left
            (0.7, SONNET, SONNET, OPUS), // 30% left
            (0.9, LOCAL, LOCAL, OPUS),   // 10% left
            (0.98, LOCAL, LOCAL, LOCAL), // 2% left
        ];
        for (used, imp, scout, top) in rows {
            let u = windowed(used);
            let m = models();
            assert_eq!(implementer_tier(Some(&u), NOW, &m, true), imp, "{used}");
            assert_eq!(scout_tier(Some(&u), NOW, &m, true), scout, "{used}");
            assert_eq!(top_tier(Some(&u), NOW, &m, true), top, "{used}");
            assert_eq!(
                mid_tier(implementer_tier(Some(&u), NOW, &m, true)),
                if imp == OPUS { SONNET } else { imp }
            );
            let down = |t: &'static str| if t == LOCAL { SONNET } else { t };
            assert_eq!(implementer_tier(Some(&u), NOW, &m, false), down(imp), "{used}");
            assert_eq!(scout_tier(Some(&u), NOW, &m, false), down(scout), "{used}");
            assert_eq!(
                top_tier(Some(&u), NOW, &m, false),
                if top == LOCAL { OPUS } else { top },
                "{used}"
            );
            assert_eq!(
                mid_tier(implementer_tier(Some(&u), NOW, &m, false)),
                down(if imp == OPUS { SONNET } else { imp })
            );
        }
    }

    #[test]
    fn custom_sonnet_above_and_pause_below_move_the_rows() {
        let mut m = models();
        m.sonnet_above_pct = 40;
        m.pause_below_pct = 30;
        let u = windowed(0.65); // 35% left
        assert_eq!(implementer_tier(Some(&u), NOW, &m, true), LOCAL);
        assert_eq!(top_tier(Some(&u), NOW, &m, true), OPUS);
        let u = windowed(0.75); // 25% left
        assert_eq!(implementer_tier(Some(&u), NOW, &m, true), LOCAL);
        assert_eq!(top_tier(Some(&u), NOW, &m, true), LOCAL);
        let u = windowed(0.5); // 50% left
        assert_eq!(implementer_tier(Some(&u), NOW, &m, true), OPUS);
        let u = windowed(0.55); // 45% left
        assert_eq!(implementer_tier(Some(&u), NOW, &m, true), SONNET);
    }

    #[test]
    fn every_row_of_the_status_only_table() {
        let st = |s: &str| json!({"status": s, "resetsAt": NOW + 1});
        let m = models();
        for avail in [true, false] {
            let local = |t: &'static str| if avail { t } else { SONNET };
            let allowed = st("allowed");
            assert_eq!(implementer_tier(Some(&allowed), NOW, &m, avail), OPUS);
            assert_eq!(scout_tier(Some(&allowed), NOW, &m, avail), SONNET);
            assert_eq!(top_tier(Some(&allowed), NOW, &m, avail), OPUS);
            let warn = st("allowed_warning");
            assert_eq!(implementer_tier(Some(&warn), NOW, &m, avail), SONNET);
            assert_eq!(scout_tier(Some(&warn), NOW, &m, avail), local(LOCAL));
            assert_eq!(top_tier(Some(&warn), NOW, &m, avail), OPUS);
            let rej = st("rejected");
            assert_eq!(implementer_tier(Some(&rej), NOW, &m, avail), local(LOCAL));
            assert_eq!(scout_tier(Some(&rej), NOW, &m, avail), local(LOCAL));
            assert_eq!(top_tier(Some(&rej), NOW, &m, avail), if avail { LOCAL } else { OPUS });
        }
    }

    #[test]
    fn implementer_model_keeps_local() {
        assert_eq!(implementer_model(LOCAL, &item(true)), LOCAL);
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
            red_sha: None,
            no_fix_sha: None,
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
