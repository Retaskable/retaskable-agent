//! docs/specs/agent-ci.md section 3.5 (the store decides the timing): the numbers the runner used to hold. The store sends them as
//! `ci: { ciHoldMinutes, ciStatusStaleMinutes, ciHeartbeatMinutes, ciDealPerTurn, ciPollSeconds }` in every `ci_merge_requests`
//! answer and in `ci_claim`'s won answer (pools-on-server.md section 14.7), the project's values or its defaults, so a change on
//! the site applies on the next poll. The runner holds no default: a repo whose timing can't be read has no check started and
//! no merge made (fail closed). Its own program: this reads nothing of the code agent.
use std::time::Duration;

use serde_json::Value;

/// One repo's timing, as the store last said it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CiTiming {
    /// How long a person's pull request waits for an up-to-date `retaskable/` pull request's check before it merges anyway; 0
    /// merges at once (`ciHoldMinutes`).
    pub hold_minutes: u32,
    /// The age at which another computer's pending status stops holding its check (`ciStatusStaleMinutes`).
    pub status_stale_minutes: u32,
    /// How often a running check's claim is refreshed (`ciHeartbeatMinutes`).
    pub heartbeat_minutes: u32,
    /// Checks started for one repo before the next repo's turn (`ciDealPerTurn`); 1 alternates repo by repo.
    pub deal_per_turn: u32,
    /// Seconds between the runner's polls (`ciPollSeconds`).
    pub poll_seconds: u32,
}

impl CiTiming {
    /// The `ci` of a store answer (`ci_merge_requests`, or `ci_claim`'s won one), or `None` when it is missing, not all there,
    /// or a number that can't be a timing (an interval of nothing).
    pub fn read(answer: &Value) -> Option<Self> {
        let ci = answer.get("ci")?;
        let n = |key: &str| ci.get(key)?.as_u64().and_then(|n| u32::try_from(n).ok());
        let timing = Self {
            hold_minutes: n("ciHoldMinutes")?,
            status_stale_minutes: n("ciStatusStaleMinutes")?,
            heartbeat_minutes: n("ciHeartbeatMinutes")?,
            deal_per_turn: n("ciDealPerTurn")?,
            poll_seconds: n("ciPollSeconds")?,
        };
        (timing.heartbeat_minutes >= 1 && timing.deal_per_turn >= 1 && timing.poll_seconds >= 1).then_some(timing)
    }

    pub fn hold(&self) -> Duration {
        Duration::from_secs(u64::from(self.hold_minutes) * 60)
    }

    /// Milliseconds past which another computer's pending status is stale.
    pub fn stale_ms(&self) -> i64 {
        i64::from(self.status_stale_minutes) * 60_000
    }

    pub fn heartbeat(&self) -> Duration {
        Duration::from_secs(u64::from(self.heartbeat_minutes) * 60)
    }

    pub fn poll(&self) -> Duration {
        Duration::from_secs(u64::from(self.poll_seconds))
    }
}

#[cfg(test)]
impl CiTiming {
    /// The store's defaults (`shared/tuning.ts`), for the tests' fake stores.
    pub fn test() -> Self {
        Self::read(&serde_json::json!({ "ci": Self::test_json() })).expect("the whole set")
    }

    pub fn test_json() -> Value {
        serde_json::json!({
            "ciHoldMinutes": 25, "ciStatusStaleMinutes": 8, "ciHeartbeatMinutes": 5, "ciDealPerTurn": 1, "ciPollSeconds": 10,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_whole_set_reads_and_anything_less_is_unknown() {
        let t = CiTiming::test();
        assert_eq!(
            (t.hold(), t.stale_ms(), t.heartbeat(), t.deal_per_turn, t.poll()),
            (
                Duration::from_secs(25 * 60),
                8 * 60_000,
                Duration::from_secs(5 * 60),
                1,
                Duration::from_secs(10)
            )
        );
        assert_eq!(CiTiming::read(&json!({})), None, "an older store sends none");
        let mut partial = CiTiming::test_json();
        partial.as_object_mut().unwrap().remove("ciPollSeconds");
        assert_eq!(CiTiming::read(&json!({ "ci": partial })), None);
        let mut zero = CiTiming::test_json();
        zero["ciHeartbeatMinutes"] = json!(0);
        assert_eq!(
            CiTiming::read(&json!({ "ci": zero })),
            None,
            "an interval of nothing is no timing"
        );
        let mut hold_zero = CiTiming::test_json();
        hold_zero["ciHoldMinutes"] = json!(0);
        assert_eq!(
            CiTiming::read(&json!({ "ci": hold_zero })).unwrap().hold(),
            Duration::ZERO,
            "0 merges at once"
        );
    }
}
