//! A chore piece (docs/specs/agent/06.7-chores-never-idle.md): a `build`/`fix` piece carrying
//! `chore: { kind, recent? }` among `RunPiece::other`. A chore's implementer runs on the chore tier, and a chore
//! never waits on a person: its own run's `questions` and `person_steps` ends become `failed`.
use serde_json::Value;

use crate::backlog::runs::{RunOutcome, RunPiece};
use crate::local_model::{LocalModel, LocalRun};
use crate::tiers::{LOCAL, chore_tier, implementer_model, implementer_tier};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoreKind {
    FixMain,
    Tidy,
    Tests,
    Bugs,
    Specs,
    SyncMain,
    PlaytestFix,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chore {
    pub kind: ChoreKind,
    /// The recent chores of this kind, one line each: the title and how it ended.
    pub recent: Vec<String>,
    /// For `fix_main`: the failing checks or the staging smoke report.
    pub report: String,
    /// For `sync_main`: the short sha of main the branch is named for, the main branch, and the conflicting files.
    pub sync_sha: String,
    pub main: String,
    pub conflicts: String,
}

impl Chore {
    /// Whether the chore has its own prompt: a playtest fix keeps the piece prompt.
    pub fn has_prompt(&self) -> bool {
        self.kind != ChoreKind::PlaytestFix
    }
}

/// A chore's prompt that couldn't be filled (our own template, not the work): the run ends `failed`, `outside`.
#[derive(Debug)]
pub struct Outside(pub String);

impl std::fmt::Display for Outside {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Outside {}

/// The piece's chore, or `None` for a roadmap piece or an unknown kind.
pub fn chore_of(piece: &RunPiece) -> Option<Chore> {
    let kind = match piece.other.get("chore")?.get("kind")?.as_str()? {
        "fix_main" => ChoreKind::FixMain,
        "tidy" => ChoreKind::Tidy,
        "tests" => ChoreKind::Tests,
        "bugs" => ChoreKind::Bugs,
        "specs" => ChoreKind::Specs,
        "sync_main" => ChoreKind::SyncMain,
        "playtest_fix" => ChoreKind::PlaytestFix,
        _ => return None,
    };
    let chore = &piece.other["chore"];
    let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let recent = chore
        .get("recent")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|r| match r.as_str() {
            Some(s) => Some(s.trim().to_string()),
            None => {
                let (title, outcome) = (text(r, "title"), text(r, "outcome"));
                (!title.is_empty()).then(|| {
                    if outcome.is_empty() {
                        title
                    } else {
                        format!("{title} ({outcome})")
                    }
                })
            }
        })
        .filter(|l| !l.is_empty())
        .collect();
    Some(Chore {
        kind,
        recent,
        report: text(chore, "report"),
        sync_sha: [chore, &Value::Object(piece.other.clone())]
            .iter()
            .map(|v| text(v, "syncSha"))
            .find(|s| !s.is_empty())
            .unwrap_or_default(),
        main: Some(text(chore, "main"))
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| "main".into()),
        conflicts: text(&Value::Object(piece.other.clone()), "remaining"),
    })
}

/// The store lock a chore of this kind holds through its run (section 6.7, pieces.md section 3.4); `None` for a
/// playtest fix, which takes none. A main sync is per base branch.
pub fn chore_lock_name(kind: ChoreKind, base: &str) -> Option<String> {
    Some(match kind {
        ChoreKind::Tidy => "chore-tidy".into(),
        ChoreKind::Tests => "chore-tests".into(),
        ChoreKind::Bugs => "chore-bugs".into(),
        ChoreKind::Specs => "chore-specs".into(),
        ChoreKind::FixMain => "fix_main".into(),
        ChoreKind::SyncMain => format!("sync_main-{base}"),
        ChoreKind::PlaytestFix => return None,
    })
}

/// The model an implementer runs on: the chore tier for a chore (a playtest fix aside), else the normal tier.
/// A `local` tier stays `local`: the engine starts it as `--model sonnet`, which its environment maps onto the model.
/// `retry` is a piece with earlier failed attempts (a `limited` one counts none), which `implementer_model` steps up.
pub fn model_for(
    chore: Option<&Chore>,
    normal_tier: &'static str,
    item: &crate::backlog::Item,
    retry: bool,
) -> &'static str {
    match chore {
        _ if normal_tier == LOCAL => LOCAL,
        Some(c) if c.kind != ChoreKind::PlaytestFix => chore_tier(),
        _ => implementer_model(normal_tier, item, u32::from(retry)),
    }
}

/// The tier a piece's implementer runs on (see [`model_for`]) for the given usage reading, with the local run it
/// holds when that tier is `local`. `local` is `None` when the session can't use the local model (it is on another
/// engine than Claude, or resumes). With `grab` (`local-chores-scouts`), a chore other than a playtest fix takes the
/// local slot only if it's free right now, else runs on the chore tier at once. With `ladder` (`local-tier-ladder`),
/// `wants_local` (the engine is set to `local`) takes the slot for any piece, waiting for it, and a roadmap piece
/// takes it as the fallback rows say, waiting for it. A chore never takes the fallback rows.
pub async fn tier_for(
    chore: Option<&Chore>,
    now: i64,
    models: &crate::config::ModelsConfig,
    local: Option<&LocalModel>,
    (grab, ladder): (bool, bool),
    wants_local: bool,
) -> (&'static str, Option<LocalRun>) {
    let up = ladder && local.is_some_and(LocalModel::available);
    if up && wants_local {
        return (LOCAL, Some(local.expect("up means there is a local model").run().await));
    }
    if chore.is_some_and(|c| c.kind != ChoreKind::PlaytestFix) {
        return match local.filter(|_| grab).and_then(LocalModel::try_run) {
            Some(run) => (LOCAL, Some(run)),
            None => (chore_tier(), None),
        };
    }
    let tier = implementer_tier(None, now, models, up);
    match local {
        Some(local) if ladder && tier == LOCAL => (tier, Some(local.run().await)),
        _ => (tier, None),
    }
}

/// A chore's own run end: `questions` and `person_steps` become `Failed`, the questions or steps kept in the why.
/// A playtest fix, a main sync and every other outcome are left as they are.
pub fn chore_end(kind: ChoreKind, outcome: RunOutcome, result: &Value) -> RunOutcome {
    if matches!(kind, ChoreKind::PlaytestFix | ChoreKind::SyncMain) {
        return outcome;
    }
    let (lead, list) = match outcome {
        RunOutcome::Questions => (
            "The chore asked questions, and a chore never waits on a person",
            "questions",
        ),
        RunOutcome::PersonSteps => (
            "The chore needs steps only a person can do, and a chore never waits",
            "steps",
        ),
        other => return other,
    };
    let items: Vec<String> = result
        .get(list)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|v| match v.get("question").unwrap_or(v) {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
        .collect();
    RunOutcome::Failed(format!("{lead}: {}", items.join(" | ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn piece(extra: Value) -> RunPiece {
        let mut p = json!({ "title": "t" });
        p.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        serde_json::from_value(p).unwrap()
    }

    #[test]
    fn chore_lock_name_for_every_kind() {
        let name = |k| chore_lock_name(k, "main");
        assert_eq!(name(ChoreKind::Tidy).as_deref(), Some("chore-tidy"));
        assert_eq!(name(ChoreKind::Tests).as_deref(), Some("chore-tests"));
        assert_eq!(name(ChoreKind::Bugs).as_deref(), Some("chore-bugs"));
        assert_eq!(name(ChoreKind::Specs).as_deref(), Some("chore-specs"));
        assert_eq!(name(ChoreKind::FixMain).as_deref(), Some("fix_main"));
        assert_eq!(name(ChoreKind::SyncMain).as_deref(), Some("sync_main-main"));
        assert_eq!(name(ChoreKind::PlaytestFix), None);
    }

    #[test]
    fn chore_of_reads_each_kind() {
        for (name, kind) in [
            ("fix_main", ChoreKind::FixMain),
            ("tidy", ChoreKind::Tidy),
            ("tests", ChoreKind::Tests),
            ("bugs", ChoreKind::Bugs),
            ("specs", ChoreKind::Specs),
            ("sync_main", ChoreKind::SyncMain),
            ("playtest_fix", ChoreKind::PlaytestFix),
        ] {
            let p = piece(json!({ "chore": { "kind": name, "recent": ["x"] } }));
            assert_eq!(chore_of(&p).map(|c| c.kind), Some(kind));
        }
    }

    #[test]
    fn every_chore_but_a_playtest_fix_has_its_own_prompt() {
        for (name, own) in [
            ("tidy", true),
            ("fix_main", true),
            ("sync_main", true),
            ("playtest_fix", false),
        ] {
            let p = piece(json!({ "chore": { "kind": name } }));
            assert_eq!(chore_of(&p).unwrap().has_prompt(), own, "{name}");
        }
    }

    #[test]
    fn no_chore_or_an_unknown_kind_is_not_a_chore() {
        assert_eq!(chore_of(&piece(json!({}))), None);
        assert_eq!(chore_of(&piece(json!({ "chore": { "kind": "juggle" } }))), None);
        assert_eq!(chore_of(&piece(json!({ "chore": {} }))), None);
    }

    fn a_chore(kind: ChoreKind) -> Chore {
        Chore {
            kind,
            recent: vec![],
            report: String::new(),
            sync_sha: String::new(),
            main: "main".into(),
            conflicts: String::new(),
        }
    }

    const NONE: (bool, bool) = (false, false);
    const GRAB: (bool, bool) = (true, false);
    const LADDER: (bool, bool) = (false, true);
    const BOTH: (bool, bool) = (true, true);

    #[tokio::test]
    async fn the_tier_is_the_chore_tier_except_for_a_playtest_fix_or_a_roadmap_piece() {
        let models = crate::config::ModelsConfig::default();
        let normal = implementer_tier(None, 0, &models, false);
        let (tidy, fix) = (a_chore(ChoreKind::Tidy), a_chore(ChoreKind::PlaytestFix));
        for local in [None, Some(&LocalModel::off())] {
            assert_eq!(
                tier_for(Some(&tidy), 0, &models, local, BOTH, false).await.0,
                chore_tier()
            );
            assert_eq!(tier_for(Some(&fix), 0, &models, local, BOTH, false).await.0, normal);
            assert_eq!(tier_for(None, 0, &models, local, BOTH, false).await.0, normal);
        }
    }

    #[tokio::test]
    async fn a_chore_takes_a_free_local_slot_and_otherwise_runs_on_the_chore_tier_at_once() {
        let models = crate::config::ModelsConfig::default();
        let tidy = a_chore(ChoreKind::Tidy);
        for flags in [GRAB, BOTH] {
            let local = LocalModel::up();
            let (tier, run) = tier_for(Some(&tidy), 0, &models, Some(&local), flags, false).await;
            assert_eq!(tier, LOCAL);
            assert_eq!(model_for(Some(&tidy), tier, &Default::default(), true), LOCAL);
            assert!(run.is_some() && local.try_take().is_none(), "the chore holds the slot");
            // Busy: no waiting.
            let (tier, run) = tier_for(Some(&tidy), 0, &models, Some(&local), flags, false).await;
            assert_eq!((tier, run.is_none()), (chore_tier(), true));
        }
    }

    #[tokio::test]
    async fn without_the_chores_flag_a_chore_never_takes_the_free_slot_even_on_the_ladder() {
        let models = crate::config::ModelsConfig::default();
        let (local, tidy) = (LocalModel::up(), a_chore(ChoreKind::Tidy));
        for flags in [NONE, LADDER] {
            let (tier, run) = tier_for(Some(&tidy), 0, &models, Some(&local), flags, false).await;
            assert_eq!((tier, run.is_none()), (chore_tier(), true));
        }
        assert!(local.try_take().is_some());
    }

    #[tokio::test]
    async fn a_chore_on_an_engine_set_to_local_waits_for_the_busy_slot_on_the_ladder_only() {
        let models = crate::config::ModelsConfig::default();
        let (local, tidy) = (LocalModel::up(), a_chore(ChoreKind::Tidy));
        let held = local.take().await;
        // The grab alone doesn't wait.
        assert_eq!(
            tier_for(Some(&tidy), 0, &models, Some(&local), GRAB, true).await.0,
            chore_tier()
        );
        let wait = tier_for(Some(&tidy), 0, &models, Some(&local), LADDER, true);
        tokio::pin!(wait);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), &mut wait)
                .await
                .is_err()
        );
        drop(held);
        assert_eq!(wait.await.0, LOCAL);
    }

    #[tokio::test]
    async fn a_playtest_fix_never_takes_the_local_slot() {
        let models = crate::config::ModelsConfig::default();
        let (local, fix) = (LocalModel::up(), a_chore(ChoreKind::PlaytestFix));
        let (_, run) = tier_for(Some(&fix), 0, &models, Some(&local), BOTH, false).await;
        assert!(run.is_none() && local.try_take().is_some());
    }

    #[tokio::test]
    async fn a_roadmap_piece_takes_the_slot_only_when_the_engine_is_local_on_the_ladder_and_it_is_up() {
        let models = crate::config::ModelsConfig::default();
        let local = LocalModel::up();
        // Above the fallback rows: no slot.
        let (tier, run) = tier_for(None, 0, &models, Some(&local), BOTH, false).await;
        assert_ne!(tier, LOCAL);
        assert!(run.is_none());
        let (tier, run) = tier_for(None, 0, &models, Some(&local), LADDER, true).await;
        assert!(tier == LOCAL && run.is_some());
        drop(run);
        // The grab alone never gives a roadmap piece the slot.
        let (_, run) = tier_for(None, 0, &models, Some(&local), GRAB, true).await;
        assert!(run.is_none());
        // Down, or no model: the role's tier.
        let normal = implementer_tier(None, 0, &models, false);
        assert_eq!(
            tier_for(None, 0, &models, Some(&LocalModel::off()), BOTH, true).await.0,
            normal
        );
        assert_eq!(tier_for(None, 0, &models, None, BOTH, true).await.0, normal);
    }

    #[test]
    fn questions_and_steps_become_failed_with_their_text() {
        let q = json!({ "questions": [{ "question": "Which?", "options": [] }, { "question": "Why?" }] });
        let s = json!({ "steps": ["Add the key.", "Restart."] });
        for kind in [
            ChoreKind::Tidy,
            ChoreKind::Tests,
            ChoreKind::Bugs,
            ChoreKind::Specs,
            ChoreKind::FixMain,
        ] {
            let RunOutcome::Failed(why) = chore_end(kind, RunOutcome::Questions, &q) else {
                panic!()
            };
            assert!(why.contains("Which?") && why.contains("Why?"), "{why}");
            let RunOutcome::Failed(why) = chore_end(kind, RunOutcome::PersonSteps, &s) else {
                panic!()
            };
            assert!(why.contains("Add the key.") && why.contains("Restart."), "{why}");
            assert_eq!(chore_end(kind, RunOutcome::PrOpened, &q), RunOutcome::PrOpened);
        }
    }

    #[test]
    fn a_playtest_fix_and_a_main_sync_are_left_alone() {
        for kind in [ChoreKind::PlaytestFix, ChoreKind::SyncMain] {
            assert_eq!(
                chore_end(kind, RunOutcome::Questions, &json!({})),
                RunOutcome::Questions
            );
            assert_eq!(
                chore_end(kind, RunOutcome::PersonSteps, &json!({})),
                RunOutcome::PersonSteps
            );
        }
    }
}
