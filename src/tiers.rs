//! Tier names and models (docs/specs/agent/07.1-model-tiers.md). The server names the tier; this maps it to a
//! model, and keeps the weekly cutoff and the usage readings it reports. No tier is worked out from usage here.
use std::sync::atomic::{AtomicI64, Ordering};

use serde_json::Value;

use crate::backlog::Item;
use crate::config::ModelsConfig;

pub const OPUS: &str = "opus";
pub const SONNET: &str = "sonnet";
pub const LOCAL: &str = "local";

/// The tier the server named for a claim (section 7.1, "How the tier arrives"): `opus`, `sonnet` or `local`.
/// A missing name is `sonnet`; an unknown one is `sonnet` too, and logged.
pub fn named_tier(name: Option<&str>) -> &'static str {
    match name {
        None | Some("sonnet") => SONNET,
        Some("opus") => OPUS,
        Some("local") => LOCAL,
        Some(other) => {
            tracing::warn!("the store named the unknown tier {other:?}; reading it as sonnet");
            SONNET
        }
    }
}

/// `tier`, or `local` when Claude is `at_limit` on this computer (`EngineLimits::claude_at_limit`) and the local
/// model is `local_available` (section 7.1: the floor row, never with the model down).
pub fn floor(tier: &'static str, at_limit: bool, local_available: bool) -> &'static str {
    if at_limit && local_available { LOCAL } else { tier }
}

/// Whether a `rate_limit_info` reading is one to keep as the usage reading (`parseRateLimit` in the TypeScript
/// agent): the 5-hour window kept as `state.usage`, or a
/// weekly rejection. Most builds report one window per event with no `rateLimitType` at all, a build that labels
/// its window keeps `"five_hour"`, and a reading with `unifiedWindows.five_hour` always counts, whatever its own
/// `rateLimitType` says. A reading labeled `"seven_day"` whose `status` is `rejected` counts too:
/// it is what turned the session away, and its reset is the week's. Anything else — an older build's lone
/// non-rejected `seven_day` event, say — would otherwise overwrite a usable 5-hour reading with one this agent
/// can't read a tier from, each time Claude Code happens to report it. A weekly reading dropped here is still
/// saved, as `state.weekUsage` (`carries_week`), so a restart keeps the weekly cutoff.
pub fn is_usage_reading(usage: &Value) -> bool {
    if usage.get("unifiedWindows").and_then(|w| w.get("five_hour")).is_some() {
        return true;
    }
    match usage.get("rateLimitType").and_then(Value::as_str) {
        Some("seven_day") => usage["status"] == "rejected",
        Some(rate_limit_type) => rate_limit_type == "five_hour",
        None => true,
    }
}

/// Whether a reading carries the weekly window (`unifiedWindows.seven_day`, or `rateLimitType: "seven_day"`): the
/// ones saved as `state.weekUsage`, whether or not `is_usage_reading` keeps them as `state.usage`.
pub fn carries_week(usage: &Value) -> bool {
    usage.get("unifiedWindows").and_then(|w| w.get("seven_day")).is_some()
        || usage.get("rateLimitType").and_then(Value::as_str) == Some("seven_day")
}

/// The weekly window of a reading: `unifiedWindows.seven_day`, else the reading itself when its `rateLimitType` is
/// `seven_day`.
fn week_window(usage: &Value) -> Option<&Value> {
    match usage.get("unifiedWindows").and_then(|w| w.get("seven_day")) {
        Some(w) => Some(w),
        None if usage.get("rateLimitType").and_then(Value::as_str) == Some("seven_day") => Some(usage),
        None => None,
    }
}

/// The week's `(utilization 0–1 read like `frac`, resetsAt)` of a reading, either shape (`week_window`); each is
/// `None` when the reading doesn't carry it (a status-only reading), and the whole is `None` with no weekly window.
pub fn week_reading(usage: &Value) -> Option<(Option<f64>, Option<i64>)> {
    let w = week_window(usage)?;
    Some((
        w.get("utilization").and_then(Value::as_f64).map(frac),
        w.get("resetsAt").and_then(Value::as_i64),
    ))
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
/// machine: a project's reading counts for every session.
#[cfg(not(test))]
static LATEST_FIVE_HOUR_RESET: AtomicI64 = AtomicI64::new(0);

// Per test thread under test, so one test's saved reading doesn't drop another test's budget.
#[cfg(test)]
thread_local! {
    static LATEST_FIVE_HOUR_RESET: AtomicI64 = const { AtomicI64::new(0) };
}

fn latest_reset() -> i64 {
    #[cfg(test)]
    return LATEST_FIVE_HOUR_RESET.with(|a| a.load(Ordering::Relaxed));
    #[cfg(not(test))]
    LATEST_FIVE_HOUR_RESET.load(Ordering::Relaxed)
}

/// Notes a project's saved usage reading in the machine-wide signal.
pub fn note_usage(usage: &Value) {
    if let Some(resets_at) = five_hour_resets_at(usage) {
        #[cfg(test)]
        LATEST_FIVE_HOUR_RESET.with(|a| a.fetch_max(resets_at, Ordering::Relaxed));
        #[cfg(not(test))]
        LATEST_FIVE_HOUR_RESET.fetch_max(resets_at, Ordering::Relaxed);
    }
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

/// The machine-wide signal alone: some project has saved a five-hour reading from the last week.
pub fn signal_on(now_epoch: i64) -> bool {
    let resets_at = latest_reset();
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

/// `implementerModel` (agent/src/orchestratorHelpers.ts in the TypeScript agent): where
/// the tier is `opus`, a roadmap item's first try instead runs on `sonnet`, so Opus is
/// kept for a reopened item, a retry (`attempt` over 0: this agent
/// counts Start over from 0) and an item of size L or XL (any case). Only a tier of `opus` is ever moved; a tier already below it (from a usage reading, section 7.1's
/// table, or `local`) is returned unchanged.
pub fn implementer_model(tier: &'static str, item: &Item, attempt: u32) -> &'static str {
    if tier != OPUS {
        return tier;
    }
    let big = item
        .size
        .as_deref()
        .is_some_and(|s| s.eq_ignore_ascii_case("L") || s.eq_ignore_ascii_case("XL"));
    if item.reopened.is_some() || attempt > 0 || big {
        OPUS
    } else {
        SONNET
    }
}

/// A usage fraction, or a whole-number percentage above 1 (`frac` in the TypeScript agent's
/// `parseRateLimit`) — Claude Code sends either shape for the same field.
pub(crate) fn frac(value: f64) -> f64 {
    if value > 1.0 { value / 100.0 } else { value }
}

/// The week's `resetsAt` (epoch seconds) when `usage` says the weekly limit is cut off now: the week's used share
/// (`unifiedWindows.seven_day.utilization`, read like `frac`) at or past `models.week_cutoff_pct`, or that window
/// `rejected`; failing a `seven_day` window, a lone reading whose `rateLimitType` is `seven_day`, read the same way
/// from its top-level fields. `None` with no reading, a reset that is missing or not after `now_epoch`, or no
/// weekly figure.
pub fn week_cutoff_until(usage: Option<&Value>, now_epoch: i64, models: &ModelsConfig) -> Option<i64> {
    let usage = usage?;
    let (used, resets_at) = week_reading(usage)?;
    let rejected = week_window(usage)?.get("status").and_then(Value::as_str) == Some("rejected");
    let cut = rejected || used.is_some_and(|u| u * 100.0 >= f64::from(models.week_cutoff_pct));
    let resets_at = resets_at?;
    (cut && resets_at > now_epoch).then_some(resets_at)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_named_tier_maps_and_an_unknown_one_is_sonnet() {
        assert_eq!(super::named_tier(Some("opus")), "opus");
        assert_eq!(super::named_tier(Some("local")), "local");
        assert_eq!(super::named_tier(Some("gpt")), "sonnet");
        assert_eq!(super::named_tier(None), "sonnet");
    }

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
    fn a_limited_claude_reads_the_floor_only_with_the_model() {
        assert_eq!(floor(OPUS, true, true), LOCAL);
        assert_eq!(floor(SONNET, true, true), LOCAL);
        assert_eq!(floor(OPUS, false, true), OPUS);
        assert_eq!(floor(OPUS, true, false), OPUS);
    }

    #[test]
    fn implementer_model_keeps_local() {
        assert_eq!(implementer_model(LOCAL, &item(true), 0), LOCAL);
    }

    fn item(reopened: bool) -> Item {
        Item {
            merge_commit: None,
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
            draft_id: None,
            reopened: reopened.then(|| crate::backlog::Reopened {
                notes: Some("n".into()),
                at: Some("2026-10-29T00:00:00Z".into()),
                by: None,
            }),
            red_sha: None,
            no_fix_sha: None,
            size: None,
            tier: None,
        }
    }

    #[test]
    fn a_first_try_at_a_full_window_runs_on_sonnet_not_opus() {
        assert_eq!(implementer_model(OPUS, &item(false), 0), SONNET);
    }

    fn sized(size: Option<&str>) -> Item {
        Item {
            size: size.map(str::to_string),
            ..item(false)
        }
    }

    #[test]
    fn retries_and_big_items_keep_opus() {
        assert_eq!(implementer_model(OPUS, &sized(None), 0), SONNET);
        assert_eq!(implementer_model(OPUS, &sized(Some("M")), 0), SONNET);
        assert_eq!(implementer_model(OPUS, &sized(None), 1), OPUS);
        assert_eq!(implementer_model(OPUS, &sized(Some("L")), 0), OPUS);
        assert_eq!(implementer_model(OPUS, &sized(Some("xl")), 0), OPUS);
        assert_eq!(implementer_model(OPUS, &item(true), 0), OPUS);
        for tier in [SONNET, LOCAL] {
            for (it, attempt) in [(item(true), 0), (sized(Some("XL")), 0), (sized(None), 2)] {
                assert_eq!(implementer_model(tier, &it, attempt), tier);
            }
        }
    }

    #[test]
    fn a_reopened_item_keeps_opus() {
        assert_eq!(implementer_model(OPUS, &item(true), 0), OPUS);
    }

    #[test]
    fn a_tier_already_below_opus_is_kept_whatever_the_item_is() {
        assert_eq!(implementer_model(SONNET, &item(true), 0), SONNET);
    }
}
