//! Why a follow-up starts a fresh session on the item's branch instead of resuming, the line the item's
//! history gets for it, and the prompt the fresh session opens with (06.2-implementer.md "Fresh-start
//! rules on resume" and "A session that's grown starts fresh").
use std::collections::HashMap;
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

/// Which engines are at their usage limit on this computer and until when. In memory only.
#[derive(Debug, Default)]
pub struct EngineLimits(HashMap<EngineId, Instant>);

impl EngineLimits {
    /// `engine` hit its limit; it counts as limited until `until`.
    pub fn hit(&mut self, engine: EngineId, until: Instant) {
        self.0.insert(engine, until);
    }

    /// Whether `engine` is limited at `now`.
    pub fn is_limited(&self, engine: EngineId, now: Instant) -> bool {
        self.0.get(&engine).is_some_and(|until| *until > now)
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
        let mut limits = EngineLimits::default();
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
        let mut limits = EngineLimits::default();
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
        let mut limits = EngineLimits::default();
        limits.hit(Claude, now + Duration::from_secs(60));
        assert!(limits.is_limited(Claude, now));
        let later = now + Duration::from_secs(61);
        assert!(!limits.is_limited(Claude, later));
        assert_eq!(limits.takeover(Claude, &[Claude, Cursor], later), None);
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
