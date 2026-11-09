//! Why a follow-up starts a fresh session on the item's branch instead of resuming, the line the item's
//! history gets for it, and the prompt the fresh session opens with (06.2-implementer.md "Fresh-start
//! rules on resume" and "A session that's grown starts fresh").
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::engines::EngineId;

/// Whether a session that started on `started_on` can't resume now (engines/03-choosing-the-engine.md section
/// 3.3): pi and Codex never resume (engines.md sections 13 and 14), and a lane that now has an engine of its
/// own, which differs, starts fresh on it. `takeover` is the engine that can take the work while `started_on`
/// is at its usage limit ([`EngineLimits::takeover`], section 3.5): the session starts fresh on it. `None`
/// resumes.
pub fn fresh_for_engine(
    started_on: EngineId,
    lane_engine: Option<EngineId>,
    takeover: Option<EngineId>,
) -> Option<FreshStart> {
    match (started_on, lane_engine, takeover) {
        (EngineId::Pi | EngineId::Codex, ..) => Some(FreshStart::CantResume(started_on)),
        (from, Some(to), _) if from != to => Some(FreshStart::LaneEngine { from, to }),
        (from, _, Some(to)) => Some(FreshStart::EngineLimited { from, to }),
        _ => None,
    }
}

/// How long an engine that hit its usage limit counts as limited when its reading gave no reset time (the
/// 30-minute wait of section 3.3).
pub const LIMIT_PAUSE: Duration = Duration::from_secs(30 * 60);

/// Which engines are at their usage limit on this computer and until when. In memory only. A cloneable handle:
/// every clone shares the same record, so the driver and the sessions that end in their own tasks write to one.
#[derive(Debug, Clone, Default)]
pub struct EngineLimits {
    until: Arc<Mutex<HashMap<EngineId, Instant>>>,
    /// The `engine-limit-records` flag: when On, any session that ends at its engine's limit records it
    /// ([`crate::orchestrator::live::sessions::record_session_end`]), whatever its role.
    any_role: Arc<AtomicBool>,
    /// The `weekly-cutoff` flag: when On, a Claude session's end at the weekly cutoff limits Claude to the week's
    /// reset, whatever its role ([`crate::orchestrator::live::sessions::record_session_end`]).
    week_cutoff: Arc<AtomicBool>,
    /// Until when Claude is held at the weekly cutoff ([`Self::cut_off_for_week`]).
    week_until: Arc<Mutex<Option<Instant>>>,
}

impl EngineLimits {
    /// `engine` hit its limit; it counts as limited until `until`, or until a later time already recorded.
    pub fn hit(&self, engine: EngineId, until: Instant) {
        let mut map = self.until.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = map.entry(engine).or_insert(until);
        *slot = (*slot).max(until);
    }

    /// Whether `engine` is limited at `now`.
    pub fn is_limited(&self, engine: EngineId, now: Instant) -> bool {
        let map = self.until.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(&engine).is_some_and(|until| *until > now)
    }

    /// Whether Claude is limited on this computer at `now`.
    pub fn claude_at_limit(&self, now: Instant) -> bool {
        self.is_limited(EngineId::Claude, now)
    }

    /// When `engine`'s limit lifts, if it is limited at `now`.
    pub fn limited_until(&self, engine: EngineId, now: Instant) -> Option<Instant> {
        let map = self.until.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(&engine).copied().filter(|until| *until > now)
    }

    /// Turns recording from every role's sessions on or off (the `engine-limit-records` flag).
    pub fn record_any_role(&self, on: bool) {
        self.any_role.store(on, Ordering::Relaxed);
    }

    /// Whether sessions of every role record their engine's limit.
    pub fn records_any_role(&self) -> bool {
        self.any_role.load(Ordering::Relaxed)
    }

    /// Turns the weekly cutoff on or off (the `weekly-cutoff` flag).
    pub fn set_week_cutoff(&self, on: bool) {
        self.week_cutoff.store(on, Ordering::Relaxed);
    }

    /// Whether the weekly cutoff is on.
    pub fn week_cutoff_on(&self) -> bool {
        self.week_cutoff.load(Ordering::Relaxed)
    }

    /// At the weekly cutoff (`tiers::week_cutoff_until`) Claude counts limited until the week's reset, so nothing
    /// new starts while running sessions finish. Whether `usage` is at the cutoff.
    pub fn cut_off_for_week(&self, usage: &serde_json::Value) -> bool {
        let models = crate::config::ModelsConfig::default();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let Some(resets_at) = crate::tiers::week_cutoff_until(Some(usage), now, &models) else {
            return false;
        };
        let until = Instant::now() + Duration::from_secs((resets_at - now) as u64);
        self.hit(EngineId::Claude, until);
        let mut week = self.week_until.lock().unwrap_or_else(PoisonError::into_inner);
        *week = (*week).max(Some(until));
        true
    }

    /// Whether Claude is held at the weekly cutoff at `now`.
    pub fn week_cut_off(&self, now: Instant) -> bool {
        let week = self.week_until.lock().unwrap_or_else(PoisonError::into_inner);
        week.is_some_and(|until| until > now)
    }

    /// The first of `candidates` (the engines the implementer role may use here) that can take over from
    /// `started_on`: only when `started_on` is itself limited, and never one that is limited too.
    pub fn takeover(&self, started_on: EngineId, candidates: &[EngineId], now: Instant) -> Option<EngineId> {
        self.is_limited(started_on, now)
            .then(|| {
                candidates
                    .iter()
                    .copied()
                    .find(|e| *e != started_on && !self.is_limited(*e, now))
            })
            .flatten()
    }
}

/// The follow-up a restarted session is given after its predecessor ran out of turns
/// (06.2-implementer.md "at most maxTurns turns").
pub const MAX_TURNS_FOLLOW_UP: &str = "An earlier session ran out of turns before it finished. Check \
    `git status` and `git diff --stat` for its uncommitted work, then carry on with the slice.";

/// A reason a resume can't carry on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshStart {
    /// The session's transcript had grown to this many tokens.
    Grown(u64),
    /// An earlier session ran out of turns.
    OutOfTurns,
    /// The item moved to a lane on another engine.
    LaneEngine { from: EngineId, to: EngineId },
    /// The engine is at its usage limit, so another takes over.
    EngineLimited { from: EngineId, to: EngineId },
    /// The engine can't resume a session at all.
    CantResume(EngineId),
}

impl FreshStart {
    /// The line the item's timeline gets.
    pub fn history_line(&self) -> String {
        match self {
            FreshStart::Grown(tokens) => format!("Its session had grown to {tokens} tokens; starting a fresh one"),
            FreshStart::OutOfTurns => "An earlier session ran out of turns; starting a fresh one".to_string(),
            FreshStart::LaneEngine { to, .. } => {
                format!("Moved to a lane on {to}, so this carries on in a fresh {to} session on the branch")
            }
            FreshStart::EngineLimited { from, to } => {
                format!("{from} is at its usage limit, so this carries on in a fresh {to} session on the branch")
            }
            FreshStart::CantResume(engine) => {
                format!("{engine} can't resume a session, so this starts a fresh one on the branch")
            }
        }
    }
}

/// The prompt a fresh session on an existing branch opens with: where the work stands, then the follow-up.
pub fn picking_up_prompt(has_pr: bool, base_branch: &str, follow_up: &str) -> String {
    let opening = if has_pr {
        "You're picking up an existing PR"
    } else {
        "You're picking up earlier work"
    };
    format!(
        "{opening}. The branch already has commits, so read `git diff --stat origin/{base_branch}...HEAD` \
         first (and `git status` for uncommitted work), then only the files that matter.\n\n{follow_up}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_reason_has_its_history_line() {
        use EngineId::*;
        assert_eq!(
            FreshStart::Grown(150_001).history_line(),
            "Its session had grown to 150001 tokens; starting a fresh one"
        );
        assert_eq!(
            FreshStart::OutOfTurns.history_line(),
            "An earlier session ran out of turns; starting a fresh one"
        );
        assert_eq!(
            FreshStart::LaneEngine {
                from: Claude,
                to: Codex
            }
            .history_line(),
            "Moved to a lane on codex, so this carries on in a fresh codex session on the branch"
        );
        assert_eq!(
            FreshStart::EngineLimited { from: Claude, to: Pi }.history_line(),
            "claude is at its usage limit, so this carries on in a fresh pi session on the branch"
        );
        assert_eq!(
            FreshStart::CantResume(Cursor).history_line(),
            "cursor can't resume a session, so this starts a fresh one on the branch"
        );
    }

    #[test]
    fn a_session_resumes_only_on_the_engine_it_started_on() {
        use EngineId::*;
        assert_eq!(fresh_for_engine(Claude, None, None), None);
        assert_eq!(fresh_for_engine(Claude, Some(Claude), None), None);
        assert_eq!(fresh_for_engine(Cursor, Some(Cursor), None), None);
        assert_eq!(
            fresh_for_engine(Claude, Some(Cursor), None),
            Some(FreshStart::LaneEngine {
                from: Claude,
                to: Cursor
            })
        );
        for engine in [Pi, Codex] {
            assert_eq!(
                fresh_for_engine(engine, None, None),
                Some(FreshStart::CantResume(engine))
            );
            assert_eq!(
                fresh_for_engine(engine, Some(engine), None),
                Some(FreshStart::CantResume(engine))
            );
        }
    }

    #[test]
    fn a_limited_engine_with_another_available_starts_fresh_on_it() {
        use EngineId::*;
        let now = Instant::now();
        let limits = EngineLimits::default();
        limits.hit(Claude, now + Duration::from_secs(60));
        let takeover = limits.takeover(Claude, &[Claude, Cursor], now);
        assert_eq!(takeover, Some(Cursor));
        assert_eq!(
            fresh_for_engine(Claude, None, takeover),
            Some(FreshStart::EngineLimited {
                from: Claude,
                to: Cursor
            })
        );
    }

    #[test]
    fn a_limited_engine_with_none_available_does_not_start_fresh() {
        use EngineId::*;
        let now = Instant::now();
        let limits = EngineLimits::default();
        limits.hit(Claude, now + Duration::from_secs(60));
        assert_eq!(limits.takeover(Claude, &[Claude], now), None);
        limits.hit(Cursor, now + Duration::from_secs(60));
        assert_eq!(limits.takeover(Claude, &[Claude, Cursor], now), None);
        assert_eq!(fresh_for_engine(Claude, None, None), None);
        // An engine that isn't limited keeps its session.
        assert_eq!(EngineLimits::default().takeover(Claude, &[Claude, Cursor], now), None);
    }

    #[test]
    fn a_limit_past_its_reset_time_no_longer_counts() {
        use EngineId::*;
        let now = Instant::now();
        let limits = EngineLimits::default();
        limits.hit(Claude, now + Duration::from_secs(60));
        assert!(limits.is_limited(Claude, now));
        let later = now + Duration::from_secs(61);
        assert!(!limits.is_limited(Claude, later));
        assert_eq!(limits.takeover(Claude, &[Claude, Cursor], later), None);
    }

    #[test]
    fn a_later_hit_never_shortens_a_limit_and_clones_share_it() {
        use EngineId::*;
        let now = Instant::now();
        let limits = EngineLimits::default();
        let clone = limits.clone();
        limits.hit(Claude, now + Duration::from_secs(600));
        clone.hit(Claude, now + Duration::from_secs(60));
        assert_eq!(limits.limited_until(Claude, now), Some(now + Duration::from_secs(600)));
        clone.hit(Claude, now + Duration::from_secs(900));
        assert_eq!(limits.limited_until(Claude, now), Some(now + Duration::from_secs(900)));
        assert!(limits.claude_at_limit(now));
        assert!(!limits.is_limited(Cursor, now));
        assert_eq!(limits.limited_until(Cursor, now), None);
        assert_eq!(limits.limited_until(Claude, now + Duration::from_secs(900)), None);
    }

    #[test]
    fn takeover_skips_the_engine_it_started_on_and_limited_candidates() {
        use EngineId::*;
        let now = Instant::now();
        let limits = EngineLimits::default();
        limits.hit(Claude, now + Duration::from_secs(60));
        limits.hit(Cursor, now + Duration::from_secs(60));
        assert_eq!(limits.takeover(Claude, &[Claude, Cursor, Codex], now), Some(Codex));
        assert_eq!(limits.takeover(Claude, &[Cursor, Codex], now), Some(Codex));
    }

    #[test]
    fn the_flags_start_off_and_can_be_turned_on_and_off() {
        let limits = EngineLimits::default();
        assert!(!limits.records_any_role());
        assert!(!limits.week_cutoff_on());
        limits.record_any_role(true);
        limits.set_week_cutoff(true);
        assert!(limits.clone().records_any_role());
        assert!(limits.clone().week_cutoff_on());
        limits.record_any_role(false);
        limits.set_week_cutoff(false);
        assert!(!limits.records_any_role());
        assert!(!limits.week_cutoff_on());
    }

    #[test]
    fn the_weekly_cutoff_limits_claude_until_the_reset() {
        let now_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let limits = EngineLimits::default();
        let now = Instant::now();
        let below =
            serde_json::json!({"unifiedWindows": {"seven_day": {"utilization": 0.10, "resetsAt": now_epoch + 3600}}});
        assert!(!limits.cut_off_for_week(&below));
        assert!(!limits.claude_at_limit(now));
        assert!(!limits.week_cut_off(now));
        let over =
            serde_json::json!({"unifiedWindows": {"seven_day": {"utilization": 0.99, "resetsAt": now_epoch + 3600}}});
        assert!(limits.cut_off_for_week(&over));
        assert!(limits.claude_at_limit(now));
        assert!(limits.week_cut_off(now));
        assert!(!limits.week_cut_off(now + Duration::from_secs(3700)));
        assert!(!limits.claude_at_limit(now + Duration::from_secs(3700)));
        // A shorter reset later doesn't pull the week's hold in.
        let sooner =
            serde_json::json!({"unifiedWindows": {"seven_day": {"utilization": 0.99, "resetsAt": now_epoch + 60}}});
        assert!(limits.cut_off_for_week(&sooner));
        assert!(limits.week_cut_off(now + Duration::from_secs(600)));
    }

    #[test]
    fn the_prompt_opens_by_whether_there_is_a_pr_and_ends_with_the_follow_up() {
        for (has_pr, opening) in [
            (true, "You're picking up an existing PR"),
            (false, "You're picking up earlier work"),
        ] {
            let prompt = picking_up_prompt(has_pr, "main", "Do the thing.");
            assert!(prompt.starts_with(opening), "{prompt}");
            assert!(prompt.contains("`git diff --stat origin/main...HEAD`"));
            assert!(prompt.contains("`git status`"));
            assert!(prompt.ends_with("Do the thing."));
        }
    }
}
