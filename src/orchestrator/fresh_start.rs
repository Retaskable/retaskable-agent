//! Why a follow-up starts a fresh session on the item's branch instead of resuming, the line the item's
//! history gets for it, and the prompt the fresh session opens with (06.2-implementer.md "Fresh-start
//! rules on resume" and "A session that's grown starts fresh").
use crate::engines::EngineId;

/// Whether a session that started on `started_on` can't resume now (engines/03-choosing-the-engine.md section
/// 3.3): pi and Codex never resume (engines.md sections 13 and 14), and a lane that now has an engine of its
/// own, which differs, starts fresh on it. `None` resumes.
pub fn fresh_for_engine(started_on: EngineId, lane_engine: Option<EngineId>) -> Option<FreshStart> {
    match (started_on, lane_engine) {
        (EngineId::Pi | EngineId::Codex, _) => Some(FreshStart::CantResume(started_on)),
        (from, Some(to)) if from != to => Some(FreshStart::LaneEngine { from, to }),
        _ => None,
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
        assert_eq!(fresh_for_engine(Claude, None), None);
        assert_eq!(fresh_for_engine(Claude, Some(Claude)), None);
        assert_eq!(fresh_for_engine(Cursor, Some(Cursor)), None);
        assert_eq!(
            fresh_for_engine(Claude, Some(Cursor)),
            Some(FreshStart::LaneEngine {
                from: Claude,
                to: Cursor
            })
        );
        for engine in [Pi, Codex] {
            assert_eq!(fresh_for_engine(engine, None), Some(FreshStart::CantResume(engine)));
            assert_eq!(
                fresh_for_engine(engine, Some(engine)),
                Some(FreshStart::CantResume(engine))
            );
        }
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
