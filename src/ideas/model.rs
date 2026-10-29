//! The pure half of ideas (Retaskable.com's `agent/src/ideas.ts`): the plain data types, the
//! session's own result shape, and [`refilled`]'s fold of a fresh round into the queue that waits
//! for a person to accept, set aside or turn down.
use std::collections::HashSet;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::text::clip;

/// `ideasQueue`'s default (shared/tuning.ts): at most this many ideas wait at a time.
pub const QUEUE: usize = 5;
/// `ideasMaxWaits`'s default (shared/tuning.ts): an idea left unanswered through this many refills
/// drops off the queue.
pub const IDEAS_MAX_WAITS_DEFAULT: u32 = 3;

/// The longest each field of a round's idea may be, and how many profiles it may be for
/// (`IDEA_LIMITS`, shared/storeIdeas.ts).
pub const IDEA_TITLE_MAX: usize = 120;
pub const IDEA_EXPERIENCE_MAX: usize = 500;
pub const IDEA_WHY_MAX: usize = 400;
pub const IDEA_WHY_NOW_MAX: usize = 300;
pub const IDEA_FOR_MAX: usize = 3;
pub const IDEA_FEATURE_MAX: usize = 40;
pub const IDEA_NEW_FEATURE_MAX: usize = 60;

/// One idea Claude suggests, waiting for a person's answer (`Idea`, shared/moreTypes.ts).
#[derive(Debug, Clone, PartialEq)]
pub struct Idea {
    pub id: String,
    pub title: String,
    /// What someone would see and do, the way a product designer describes it.
    pub experience: String,
    pub why: String,
    pub why_now: String,
    pub for_profiles: Vec<String>,
    /// The feature on the Features page it changes (its id), or the new feature it would add
    /// (`new_feature`, a name) — never both.
    pub feature: Option<String>,
    pub new_feature: Option<String>,
    pub waited: u32,
}

/// What [`refilled`] folds a new round into: the ideas already waiting, and the titles already
/// answered, so a fresh round never repeats them. Trimmed from shared/moreTypes.ts's `IdeaList`: no
/// `state`/`generatedAt`/`people`, which need the orchestrator and the shared backlog this agent
/// doesn't have yet.
#[derive(Debug, Clone, Default)]
pub struct Queue {
    pub ideas: Vec<Idea>,
    pub accepted_titles: Vec<String>,
    pub never: Vec<String>,
}

/// One idea the session suggested, as `IDEAS_RESULT_SCHEMA`'s items shape it (agent/src/schemas.ts).
#[derive(Debug, Clone, Deserialize)]
pub struct IdeaSuggestion {
    pub title: String,
    #[serde(default)]
    pub experience: String,
    #[serde(default)]
    pub why: String,
    #[serde(default, rename = "whyNow")]
    pub why_now: String,
    #[serde(default, rename = "for")]
    pub for_profiles: Vec<String>,
    #[serde(default)]
    pub feature: String,
    #[serde(default, rename = "newFeature")]
    pub new_feature: String,
}

/// `IdeasResult` (agent/src/schemas.ts): the structured answer an ideas session returns.
#[derive(Debug, Clone, Deserialize)]
pub struct IdeasResult {
    pub status: String,
    #[serde(default)]
    pub ideas: Vec<IdeaSuggestion>,
    #[serde(default)]
    pub error: Option<String>,
}

/// `IDEAS_RESULT_SCHEMA` (agent/src/schemas.ts), for a session's `schema`.
pub fn ideas_result_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "status": { "enum": ["ok", "failed"] },
            "ideas": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "title": { "type": "string" },
                        "experience": { "type": "string" },
                        "why": { "type": "string" },
                        "whyNow": { "type": "string" },
                        "for": { "type": "array", "items": { "type": "string" } },
                        "feature": { "type": "string" },
                        "newFeature": { "type": "string" }
                    },
                    "required": ["title", "experience", "why", "whyNow", "for"]
                }
            },
            "error": { "type": "string" }
        },
        "required": ["status", "ideas"]
    })
}

/// The queue after a refill (`refilled`, agent/src/ideas.ts): the ideas still waiting (one more
/// refill each, the long-waiting ones dropped), then the new ones the session gave, up to `queue` in
/// all: none already waiting, accepted, or answered Never, each naming only known profiles (at most
/// [`IDEA_FOR_MAX`], once each), with a fresh id built from `stamp`.
pub fn refilled(
    was: &Queue,
    res: &IdeasResult,
    profiles: &[String],
    stamp: &str,
    queue: usize,
    max_waited: u32,
) -> Vec<Idea> {
    let waiting: Vec<Idea> = was
        .ideas
        .iter()
        .cloned()
        .map(|i| Idea {
            waited: i.waited + 1,
            ..i
        })
        .filter(|i| i.waited < max_waited)
        .collect();

    let mut taken: HashSet<String> = was.never.iter().map(|t| t.trim().to_lowercase()).collect();
    taken.extend(waiting.iter().map(|i| i.title.trim().to_lowercase()));
    taken.extend(was.accepted_titles.iter().map(|t| t.trim().to_lowercase()));

    let known: HashSet<&str> = profiles.iter().map(|s| s.as_str()).collect();
    let room = queue.saturating_sub(waiting.len());

    let fresh: Vec<Idea> = res
        .ideas
        .iter()
        .filter(|i| !i.title.trim().is_empty())
        .filter(|i| !taken.contains(&i.title.trim().to_lowercase()))
        .take(room)
        .enumerate()
        .map(|(n, i)| {
            let mut for_profiles = Vec::new();
            for p in &i.for_profiles {
                if known.contains(p.as_str()) && !for_profiles.contains(p) {
                    for_profiles.push(p.clone());
                }
            }
            for_profiles.truncate(IDEA_FOR_MAX);
            let feature = (!i.feature.trim().is_empty()).then(|| clip(i.feature.trim(), IDEA_FEATURE_MAX));
            let new_feature = (feature.is_none() && !i.new_feature.trim().is_empty())
                .then(|| clip(i.new_feature.trim(), IDEA_NEW_FEATURE_MAX));
            Idea {
                id: format!("{stamp}-{n}"),
                title: clip(i.title.trim(), IDEA_TITLE_MAX),
                experience: clip(i.experience.trim(), IDEA_EXPERIENCE_MAX),
                why: clip(i.why.trim(), IDEA_WHY_MAX),
                why_now: clip(i.why_now.trim(), IDEA_WHY_NOW_MAX),
                for_profiles,
                feature,
                new_feature,
                waited: 0,
            }
        })
        .collect();

    waiting.into_iter().chain(fresh).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ideas::test_support::waiting_idea;

    fn res(titles: &[&str]) -> IdeasResult {
        IdeasResult {
            status: "ok".to_string(),
            ideas: titles
                .iter()
                .map(|title| IdeaSuggestion {
                    title: title.to_string(),
                    experience: "You tap it.".to_string(),
                    why: "Because.".to_string(),
                    why_now: "It just went live.".to_string(),
                    for_profiles: vec!["shopper", "nobody", "shopper", "admin", "guest", "analyst"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    feature: if *title == "B" {
                        "inbox".to_string()
                    } else {
                        String::new()
                    },
                    new_feature: String::new(),
                })
                .collect(),
            error: None,
        }
    }

    fn profiles(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn refill_keeps_at_most_five_waiting_already_waiting_first_then_new_ones() {
        let was = Queue {
            ideas: vec![waiting_idea("Old", 0)],
            never: vec!["C".to_string()],
            accepted_titles: vec!["D".to_string()],
        };
        let next = refilled(
            &was,
            &res(&["old", "  ", "C", "D", "A", "B", "E", "F", "G"]),
            &profiles(&["shopper", "admin", "guest", "analyst"]),
            "t",
            QUEUE,
            IDEAS_MAX_WAITS_DEFAULT,
        );
        assert_eq!(
            next.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
            vec!["Old", "A", "B", "E", "F"],
            "not the waiting one again, nor Never, nor accepted"
        );
        assert_eq!(next.len(), QUEUE);
        assert_eq!(next[0].waited, 1, "a waiting idea sits through one more refill");
        assert_eq!(
            next[1].for_profiles,
            vec!["shopper", "admin", "guest"],
            "known user types, once each, at most three"
        );
        assert_eq!(next[1].why_now, "It just went live.");
        assert_eq!(next[2].feature, Some("inbox".to_string()));
        assert_eq!(next[1].experience, "You tap it.");
    }

    #[test]
    fn an_idea_left_unanswered_through_three_refills_drops_off() {
        let was = Queue {
            ideas: vec![waiting_idea("Stale", IDEAS_MAX_WAITS_DEFAULT - 1)],
            never: vec![],
            accepted_titles: vec![],
        };
        let next = refilled(&was, &res(&["A"]), &[], "t", QUEUE, IDEAS_MAX_WAITS_DEFAULT);
        assert_eq!(next.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(), vec!["A"]);

        let kept = refilled(&was, &res(&["A"]), &[], "t", QUEUE, IDEAS_MAX_WAITS_DEFAULT + 1);
        assert_eq!(
            kept.iter().map(|i| i.title.as_str()).collect::<Vec<_>>(),
            vec!["Stale", "A"],
            "a higher max lets it wait one refill more"
        );
    }

    #[test]
    fn a_refilled_idea_stays_within_the_stores_own_limits() {
        let long = |n: usize| "x".repeat(n * 2);
        let r = IdeasResult {
            status: "ok".to_string(),
            ideas: vec![IdeaSuggestion {
                title: long(IDEA_TITLE_MAX),
                experience: long(IDEA_EXPERIENCE_MAX),
                why: long(IDEA_WHY_MAX),
                why_now: long(IDEA_WHY_NOW_MAX),
                for_profiles: vec!["a", "b", "c", "d"].into_iter().map(String::from).collect(),
                feature: long(IDEA_FEATURE_MAX),
                new_feature: String::new(),
            }],
            error: None,
        };
        let next = refilled(
            &Queue::default(),
            &r,
            &profiles(&["a", "b", "c", "d"]),
            "t",
            QUEUE,
            IDEAS_MAX_WAITS_DEFAULT,
        );
        let idea = &next[0];
        assert_eq!(idea.for_profiles.len(), IDEA_FOR_MAX);
        assert_eq!(idea.title.chars().count(), IDEA_TITLE_MAX);
        assert_eq!(idea.experience.chars().count(), IDEA_EXPERIENCE_MAX);
        assert_eq!(idea.why.chars().count(), IDEA_WHY_MAX);
        assert_eq!(idea.why_now.chars().count(), IDEA_WHY_NOW_MAX);
        assert_eq!(idea.feature.as_ref().unwrap().chars().count(), IDEA_FEATURE_MAX);
    }
}
