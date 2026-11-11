//! Whether a plan goes ahead on the yes its requester already gave (agent/06.5-spec-drafts.md, simple-flow.md
//! section 2): `approvesItself` in the TypeScript agent's `drafts.ts`.
use crate::config::Plans;
use crate::text::is_blank;

/// A plan someone asked for goes ahead on that yes, unless the drafter marked it `risk`, the project's `plans` is
/// `ask`, or it is a trial draft (first-run-check.md: a trial never approves itself). A plan nobody asked for and a
/// release note's always wait.
pub fn approves_itself(
    plans: Plans,
    requested_by: Option<&str>,
    risk: Option<&str>,
    release: bool,
    trial: bool,
) -> bool {
    plans == Plans::Risky
        && requested_by.is_some_and(|b| !b.is_empty())
        && risk.is_none_or(is_blank)
        && !release
        && !trial
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_requested_plan_approves_itself() {
        assert!(approves_itself(Plans::Risky, Some("ana"), None, false, false));
        assert!(approves_itself(Plans::Risky, Some("ana"), Some("  "), false, false));
    }

    #[test]
    fn risk_ask_trial_release_and_no_requester_wait() {
        let risk = Some("It changes how people sign in.");
        assert!(!approves_itself(Plans::Risky, Some("ana"), risk, false, false));
        assert!(!approves_itself(Plans::Ask, Some("ana"), None, false, false));
        assert!(!approves_itself(Plans::Risky, Some("ana"), None, false, true));
        assert!(!approves_itself(Plans::Risky, Some("ana"), None, true, false));
        assert!(!approves_itself(Plans::Risky, None, None, false, false));
    }
}
