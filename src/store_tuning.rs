//! The numbers the store holds for the agent (pools-on-server.md section 14.6, `AGENT_NUMBERS` in Retaskable.com's
//! `shared/tuning.ts`): each the project's value or the store's default, sent in `health`'s answer as `tuning`. The agent holds
//! none of its own. An answer without them (an older store, or one that can't be read) leaves them unknown, and what needs one
//! waits: nothing is started without them (fail closed).
use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

/// Every number of `AGENT_NUMBERS`, whole. No `Default`: there is no number of the agent's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreTuning {
    /// Times an implementer's question a person already answered is sent back.
    pub max_reasks: u32,
    /// Merge: retries while the host says it isn't ready.
    pub max_merge_retries: u32,
    /// CI: reruns of a run that was cancelled.
    pub max_ci_reruns: u32,
    /// CI: minutes a run may stay pending before a person is asked.
    pub ci_max_minutes: u32,
    /// CI: minutes to wait for any check to be reported before carrying on without CI.
    pub ci_grace_minutes: u32,
    /// Watchdog: minutes a session prints nothing before it's killed and resumed.
    pub session_quiet_minutes: u32,
    /// Minutes every session on an engine waits after its usage limit that gave no reset time.
    pub cursor_limit_wait_minutes: u32,
    /// Weekly Claude limit: percent used at which nothing new starts until the week resets.
    pub week_cutoff_pct: u32,
    /// Own failures in a row retried or handed back before the next goes to a person.
    pub own_failure_retries: u32,
    /// Checks: seconds between asks for main's results.
    pub baseline_wait_seconds: u32,
    /// Checks: minutes failing checks wait for main's results before they count against the change.
    pub baseline_give_up_minutes: u32,
    /// Main's CI: seconds before a pending run on the default branch is read again.
    pub main_ci_read_seconds: u32,
    /// Minutes production is given to serve a pushed head.
    pub deploy_wait_minutes: u32,
    /// Spec drafts: tries at the merge once the branch is rebased.
    pub draft_merge_tries: u32,
    /// Spec drafts: times a conflicted draft goes back to its drafter to rebase.
    pub draft_hand_backs: u32,
}

impl StoreTuning {
    /// The numbers of a `health` answer's `tuning`, or `None` when it is missing or not all there.
    pub fn read(value: &Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }

    /// `deserialize_with` for the answer's `tuning`: anything that is not the whole set reads as unknown.
    pub fn lenient<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Self>, D::Error> {
        Ok(Option::<Value>::deserialize(d)?.as_ref().and_then(Self::read))
    }

    pub fn limit_wait(&self) -> Duration {
        minutes(self.cursor_limit_wait_minutes)
    }

    pub fn baseline_every(&self) -> Duration {
        Duration::from_secs(u64::from(self.baseline_wait_seconds))
    }

    pub fn baseline_give_up(&self) -> Duration {
        minutes(self.baseline_give_up_minutes)
    }

    pub fn main_ci_every(&self) -> Duration {
        Duration::from_secs(u64::from(self.main_ci_read_seconds))
    }

    pub fn deploy_wait(&self) -> Duration {
        minutes(self.deploy_wait_minutes)
    }
}

fn minutes(m: u32) -> Duration {
    Duration::from_secs(u64::from(m) * 60)
}

#[cfg(test)]
impl StoreTuning {
    /// The store's defaults (`shared/tuning.ts`), for the tests' fake stores and rigs: what a project that tunes nothing gets.
    pub fn test() -> Self {
        Self::read(&Self::test_json()).expect("the whole set")
    }

    pub fn test_json() -> Value {
        serde_json::json!({
            "maxReasks": 2, "maxMergeRetries": 5, "maxCiReruns": 3, "ciMaxMinutes": 120, "ciGraceMinutes": 10,
            "sessionQuietMinutes": 20, "cursorLimitWaitMinutes": 30, "weekCutoffPct": 95, "ownFailureRetries": 3,
            "baselineWaitSeconds": 30, "baselineGiveUpMinutes": 10, "mainCiReadSeconds": 60, "deployWaitMinutes": 20,
            "draftMergeTries": 5, "draftHandBacks": 2,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_whole_set_reads_and_a_partial_or_missing_one_is_unknown() {
        assert_eq!(StoreTuning::read(&StoreTuning::test_json()).unwrap().max_reasks, 2);
        let mut partial = StoreTuning::test_json();
        partial.as_object_mut().unwrap().remove("deployWaitMinutes");
        assert_eq!(
            StoreTuning::read(&partial),
            None,
            "one missing number is no set: nothing is guessed"
        );
        assert_eq!(StoreTuning::read(&json!(null)), None);
        assert_eq!(StoreTuning::read(&json!({ "maxReasks": "two" })), None);
    }

    #[test]
    fn the_numbers_are_the_stores_in_the_units_each_use_wants() {
        let t = StoreTuning::test();
        assert_eq!(t.limit_wait(), Duration::from_secs(30 * 60));
        assert_eq!(t.baseline_every(), Duration::from_secs(30));
        assert_eq!(t.baseline_give_up(), Duration::from_secs(10 * 60));
        assert_eq!(t.main_ci_every(), Duration::from_secs(60));
        assert_eq!(t.deploy_wait(), Duration::from_secs(20 * 60));
    }
}
