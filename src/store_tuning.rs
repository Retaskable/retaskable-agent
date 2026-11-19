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
    /// Sessions killed for silence and resumed by id before the next goes to a person.
    pub max_silence_resumes: u32,
    /// Times a Cursor session may hit its usage limit before the item is handed to a person.
    pub max_cursor_limits: u32,
    /// Seconds before a session killed for silence is resumed.
    pub silence_resume_seconds: u32,
    /// Seconds Claude being signed out pauses new starts before it is looked at again.
    pub signed_out_retry_seconds: u32,
    /// Merge: minutes the host may say "unknown" before the branch is updated.
    pub merge_unknown_minutes: u32,
    /// Merge: seconds before a merge the host wasn't ready for is tried again.
    pub merge_retry_seconds: u32,
    /// CI watch: host errors in a row before the watch ends failed.
    pub gh_errors_max: u32,
    /// Minutes a review that hit a usage limit (with no reset time) waits.
    pub review_limit_wait_minutes: u32,
    /// Seconds between an agent's `health` reports (10 to 600). A cadence: an older store that sends none leaves the default.
    #[serde(default = "default_report")]
    pub report_seconds: u32,
    /// Seconds between reads of a piece's CI checks (15 to 600). A cadence.
    #[serde(default = "default_watch")]
    pub watch_seconds: u32,
    /// Seconds between the CI watcher's passes (30 to 900). A cadence.
    #[serde(default = "default_ci_watch")]
    pub ci_watch_seconds: u32,
    /// Seconds between asks while the merge-turn holder waits on CI or the host (5 to 300). A cadence.
    #[serde(default = "default_merge_poll")]
    pub merge_poll_seconds: u32,
}

const fn default_report() -> u32 {
    60
}
const fn default_watch() -> u32 {
    45
}
const fn default_ci_watch() -> u32 {
    120
}
const fn default_merge_poll() -> u32 {
    15
}

/// The agent's own beats (`reportSeconds`, `watchSeconds`, `ciWatchSeconds`, `mergePollSeconds`), each the store's seconds or,
/// when the store sent none, sent one out of range, or isn't known yet, the store's default: a beat is only a pace, so unlike
/// the other numbers it never waits for the store (store-push-now.md section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cadences {
    pub report: Duration,
    pub watch: Duration,
    pub ci_watch: Duration,
    pub merge_poll: Duration,
}

impl Cadences {
    pub fn of(tuning: Option<StoreTuning>) -> Self {
        let t = tuning;
        let pick = |v: Option<u32>, def: u32, min: u32, max: u32| {
            Duration::from_secs(u64::from(v.filter(|v| (min..=max).contains(v)).unwrap_or(def)))
        };
        Self {
            report: pick(t.map(|t| t.report_seconds), 60, 10, 600),
            watch: pick(t.map(|t| t.watch_seconds), 45, 15, 600),
            ci_watch: pick(t.map(|t| t.ci_watch_seconds), 120, 30, 900),
            merge_poll: pick(t.map(|t| t.merge_poll_seconds), 15, 5, 300),
        }
    }
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

    pub fn silence_resume(&self) -> Duration {
        Duration::from_secs(u64::from(self.silence_resume_seconds))
    }

    pub fn signed_out_retry(&self) -> Duration {
        Duration::from_secs(u64::from(self.signed_out_retry_seconds))
    }

    pub fn merge_unknown(&self) -> Duration {
        minutes(self.merge_unknown_minutes)
    }

    pub fn merge_retry(&self) -> Duration {
        Duration::from_secs(u64::from(self.merge_retry_seconds))
    }

    pub fn review_limit_wait(&self) -> Duration {
        minutes(self.review_limit_wait_minutes)
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
            "draftMergeTries": 5, "draftHandBacks": 2, "maxSilenceResumes": 3, "maxCursorLimits": 3, "silenceResumeSeconds": 60,
            "signedOutRetrySeconds": 60, "mergeUnknownMinutes": 5, "mergeRetrySeconds": 30, "ghErrorsMax": 5, "reviewLimitWaitMinutes": 10,
            "reportSeconds": 60, "watchSeconds": 45, "ciWatchSeconds": 120, "mergePollSeconds": 15,
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
    fn a_store_that_sends_no_cadence_leaves_the_defaults_and_one_out_of_range_reads_as_the_default() {
        let mut old = StoreTuning::test_json();
        for k in ["reportSeconds", "watchSeconds", "ciWatchSeconds", "mergePollSeconds"] {
            old.as_object_mut().unwrap().remove(k);
        }
        let t = StoreTuning::read(&old).expect("the other numbers are all there");
        assert_eq!(Cadences::of(Some(t)), Cadences::of(None));
        let c = Cadences::of(None);
        assert_eq!(
            [c.report, c.watch, c.ci_watch, c.merge_poll].map(|d| d.as_secs()),
            [60, 45, 120, 15]
        );
        let mut tuned = StoreTuning::test_json();
        tuned["watchSeconds"] = json!(90);
        tuned["ciWatchSeconds"] = json!(5);
        tuned["mergePollSeconds"] = json!(20);
        tuned["reportSeconds"] = json!(30);
        let c = Cadences::of(StoreTuning::read(&tuned));
        assert_eq!(
            [c.report, c.watch, c.ci_watch, c.merge_poll].map(|d| d.as_secs()),
            [30, 90, 120, 20]
        );
    }

    #[test]
    fn the_numbers_are_the_stores_in_the_units_each_use_wants() {
        let t = StoreTuning::test();
        assert_eq!(t.limit_wait(), Duration::from_secs(30 * 60));
        assert_eq!(t.baseline_every(), Duration::from_secs(30));
        assert_eq!(t.baseline_give_up(), Duration::from_secs(10 * 60));
        assert_eq!(t.main_ci_every(), Duration::from_secs(60));
        assert_eq!(t.deploy_wait(), Duration::from_secs(20 * 60));
        assert_eq!(t.silence_resume(), Duration::from_secs(60));
        assert_eq!(t.signed_out_retry(), Duration::from_secs(60));
        assert_eq!(t.merge_unknown(), Duration::from_secs(5 * 60));
        assert_eq!(t.merge_retry(), Duration::from_secs(30));
        assert_eq!(t.review_limit_wait(), Duration::from_secs(10 * 60));
    }
}
