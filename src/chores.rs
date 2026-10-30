//! Rotation chores (docs/specs/agent/06.7-chores-never-idle.md): tidy-ups, tests, bug hunts and spec
//! drafts the agent picks up on its own when nothing on the roadmap can run. The store decides which
//! kind runs next and counts the day against its own limit (`backlog::Client::chore_next`); this
//! agent keeps no rotation or day count of its own, only a running id counter and the short history
//! of recent chores so a session doesn't repeat one, since code chores are local to this agent and
//! never written to the shared backlog (backlog/07-what-changes-from-today.md: "Still local to the
//! agent"). `fix_main`, `sync_main` and a playtest fix are separate, store-side chores that bypass
//! this rotation and aren't modeled here.
use serde::{Deserialize, Serialize};

/// A rotation chore's kind, in the order the store rotates them ("tidy → tests → bugs → specs").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoreKind {
    Tidy,
    Tests,
    Bugs,
    Specs,
}

impl ChoreKind {
    /// Reads the kind `chore_next` answered (its lowercase wire word), or `None` for a word this
    /// agent doesn't know (a future kind the store added, say): the caller treats that as nothing
    /// to run rather than guessing.
    pub fn parse(kind: &str) -> Option<Self> {
        match kind {
            "tidy" => Some(Self::Tidy),
            "tests" => Some(Self::Tests),
            "bugs" => Some(Self::Bugs),
            "specs" => Some(Self::Specs),
            _ => None,
        }
    }

    /// The wire word `chore_next` and `chore_lock` use for this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tidy => "tidy",
            Self::Tests => "tests",
            Self::Bugs => "bugs",
            Self::Specs => "specs",
        }
    }

    /// A short, person-readable label for the chore item's title (`chore-<n>: <label>`).
    pub fn label(self) -> &'static str {
        match self {
            Self::Tidy => "Tidy up",
            Self::Tests => "Write tests",
            Self::Bugs => "Fix a bug",
            Self::Specs => "Write a spec",
        }
    }

    /// What the session is asked to do (`prompts/chore.md`'s `{{choreInstructions}}`).
    pub fn instructions(self) -> &'static str {
        match self {
            Self::Tidy => {
                "Find one coherent area with warnings, dead code, duplication or an over-long \
                 function, and clean it up. Make no behaviour change."
            }
            Self::Tests => {
                "Find one important piece of behaviour with no test, preferring pure logic over \
                 anything that needs a server or a browser, and write a test for it."
            }
            Self::Bugs => {
                "Find one bug you can prove, and fix it at its root cause with a test that failed \
                 before your fix and passes after."
            }
            Self::Specs => {
                "Find one thin or outdated spec and flesh it out to describe the behaviour as it \
                 is. This is a draft: a person approves it before it merges."
            }
        }
    }
}

/// One finished chore, kept so a later session of the same kind doesn't repeat it, and for the
/// person-visible history ("The last 30 finished chores are kept").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoreRecord {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub outcome: String,
    pub at: String,
}

/// How many finished chores [`ChoreRoster::finished`] keeps.
const MAX_HISTORY: usize = 30;

/// The local roster this agent keeps for chores: the next `chore-<n>` id, and recent history,
/// pruned to the last 30. Everything else about the rotation (its order, the day's count and
/// limit) is the store's, asked fresh each time with `chore_next`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoreRoster {
    #[serde(default)]
    pub next_id: u32,
    #[serde(default)]
    pub history: Vec<ChoreRecord>,
}

impl ChoreRoster {
    /// The next `chore-<n>` id, advancing the counter.
    pub fn take_id(&mut self) -> String {
        self.next_id += 1;
        format!("chore-{}", self.next_id)
    }

    /// This kind's recent chore titles, newest first, for the prompt's "don't repeat yourself" list.
    pub fn recent(&self, kind: ChoreKind) -> Vec<&str> {
        self.history
            .iter()
            .rev()
            .filter(|r| r.kind == kind.as_str())
            .map(|r| r.title.as_str())
            .collect()
    }

    /// Records a finished chore, pruning the oldest once there are more than [`MAX_HISTORY`].
    pub fn finished(&mut self, record: ChoreRecord) {
        self.history.push(record);
        if self.history.len() > MAX_HISTORY {
            let drop = self.history.len() - MAX_HISTORY;
            self.history.drain(0..drop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chore_kind_round_trips_through_its_wire_word() {
        for kind in [ChoreKind::Tidy, ChoreKind::Tests, ChoreKind::Bugs, ChoreKind::Specs] {
            assert_eq!(ChoreKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(ChoreKind::parse("fix_main"), None);
        assert_eq!(ChoreKind::parse(""), None);
    }

    #[test]
    fn take_id_counts_up_from_one_and_never_repeats() {
        let mut roster = ChoreRoster::default();
        assert_eq!(roster.take_id(), "chore-1");
        assert_eq!(roster.take_id(), "chore-2");
        assert_eq!(roster.take_id(), "chore-3");
    }

    #[test]
    fn recent_lists_only_the_kinds_titles_newest_first() {
        let mut roster = ChoreRoster::default();
        roster.finished(record("chore-1", ChoreKind::Tidy, "Tidied the config module"));
        roster.finished(record("chore-2", ChoreKind::Tests, "Tested the lane pool"));
        roster.finished(record("chore-3", ChoreKind::Tidy, "Tidied the backlog client"));

        assert_eq!(
            roster.recent(ChoreKind::Tidy),
            vec!["Tidied the backlog client", "Tidied the config module"]
        );
        assert_eq!(roster.recent(ChoreKind::Bugs), Vec::<&str>::new());
    }

    #[test]
    fn finished_prunes_down_to_the_last_thirty() {
        let mut roster = ChoreRoster::default();
        for n in 0..35 {
            roster.finished(record(&format!("chore-{n}"), ChoreKind::Tidy, &format!("Chore {n}")));
        }
        assert_eq!(roster.history.len(), 30);
        assert_eq!(roster.history.first().unwrap().title, "Chore 5");
        assert_eq!(roster.history.last().unwrap().title, "Chore 34");
    }

    fn record(id: &str, kind: ChoreKind, title: &str) -> ChoreRecord {
        ChoreRecord {
            id: id.to_string(),
            kind: kind.as_str().to_string(),
            title: title.to_string(),
            outcome: "merged".to_string(),
            at: "2026-10-29T00:00:00Z".to_string(),
        }
    }
}
