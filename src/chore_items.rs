//! A chore task as a local item (docs/specs/pools.md section 3d; the TypeScript agent's `addTaskChore` in `chores.ts`,
//! `addFixChore` in `fixTasks.ts`, `startSync` in `mainSync.ts`). The item is this computer's vehicle for the task: its
//! lane, branch, session and change, run through the same item flow as a claimed item. Its id is the task's, no store
//! item stands behind it, so it holds no lease and the loop writes nothing to the store for it (progress, park,
//! completion); the merge turn is asked for as a chore entry. Pure: the driver keeps them (`driver::chores`).
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::backlog::pool::TaskInput;
use crate::backlog::runs::RunPiece;
use crate::backlog::{Item, ItemStatus};
use crate::implementer::prompt::chore_label;
use crate::runs::chore::{Chore, ChoreKind};

/// The milestone a chore item is listed under (`milestoneId: "chores"`).
pub const MILESTONE: &str = "chores";

/// What a main sync's branch and merge need beyond a session chore (`syncSha`, the conflicting files).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncWork {
    /// Main's commit the sync merges, whole.
    pub sha: String,
    /// The default branch the base catches up with.
    pub main: String,
    /// The files that conflict, in the words the session is told; empty when the merge was clean.
    #[serde(default)]
    pub conflicts: String,
}

/// A chore item's own facts, kept in the state file (`state.chores`) so a restart takes it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoreWork {
    /// `fix_main`, `fix_playtest`, `tidy`, `tests`, `bugs` or `sync_main`.
    pub kind: String,
    /// The words the store sent (the task's prompt): the session's instructions as they are.
    #[serde(default)]
    pub prompt: String,
    pub title: String,
    /// On a `fix_main`: the base commit it was queued for, so a fix that ends with nothing to fix is told to the store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub red_sha: Option<String>,
    /// A playtest fix counts toward the item the first try-out was of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixes_item: Option<String>,
    /// A rollback's revert: runs beyond the computer's session limit.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub extra_slot: bool,
    /// The tier the store picked (`opus`, `sonnet`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncWork>,
}

/// The chore kind a name stands for.
pub fn kind_of(name: &str) -> Option<ChoreKind> {
    Some(match name {
        "fix_main" => ChoreKind::FixMain,
        "fix_playtest" | "playtest_fix" => ChoreKind::PlaytestFix,
        "tidy" => ChoreKind::Tidy,
        "tests" => ChoreKind::Tests,
        "bugs" => ChoreKind::Bugs,
        "specs" => ChoreKind::Specs,
        "sync_main" => ChoreKind::SyncMain,
        _ => return None,
    })
}

/// The name a kind is kept and sent under.
pub fn name_of(kind: ChoreKind) -> &'static str {
    match kind {
        ChoreKind::FixMain => "fix_main",
        ChoreKind::PlaytestFix => "fix_playtest",
        ChoreKind::Tidy => "tidy",
        ChoreKind::Tests => "tests",
        ChoreKind::Bugs => "bugs",
        ChoreKind::Specs => "specs",
        ChoreKind::SyncMain => "sync_main",
    }
}

/// The kinds a task becomes a local item for: the rotation's code chores and the fixes (`ROTATION` and `fix_main` in
/// `poolTasks.ts`; `specs` is a draft and `sync_main` is made by [`sync_work`]).
pub fn session_kind(name: &str) -> Option<ChoreKind> {
    kind_of(name).filter(|k| !matches!(k, ChoreKind::Specs | ChoreKind::SyncMain))
}

/// A chore item's work for `task` of `kind`; `base_sha` is the base branch's commit now (a fix main's `redSha`).
/// The title is `<label>: <the fix's title or the subject>` for a fix and `<label>: finding something worth doing` for a
/// rotation chore, as `addFixChore` and `addTaskChore` have it.
pub fn work_of(task: &TaskInput, kind: ChoreKind, base_sha: Option<&str>) -> ChoreWork {
    let label = chore_label(kind);
    let fix = matches!(kind, ChoreKind::PlaytestFix) || task.fix.is_some() || task.subject.starts_with("fix_smoke:");
    let title = match (&task.fix, kind) {
        (Some(f), _) if !f.title.trim().is_empty() => format!("{label}: {}", f.title.trim()),
        _ if fix => format!("{label}: {}", task.subject),
        (_, ChoreKind::FixMain) => format!("{label}: main's checks failing"),
        _ => format!("{label}: finding something worth doing"),
    };
    ChoreWork {
        kind: name_of(kind).to_string(),
        prompt: task.prompt.trim().to_string(),
        title,
        red_sha: (kind == ChoreKind::FixMain && !task.subject.starts_with("fix_smoke:"))
            .then(|| base_sha.map(str::to_string))
            .flatten(),
        fixes_item: (kind == ChoreKind::PlaytestFix)
            .then(|| task.fix.as_ref().and_then(|f| f.item_id.clone()))
            .flatten(),
        extra_slot: kind == ChoreKind::FixMain && task.extra_slot,
        tier: Some(task.tier.item_tier().to_string()),
        sync: None,
    }
}

/// A main sync's work: its branch is named for main's commit (`syncBranch`), merged clean or with conflicts left to a session.
pub fn sync_work(task: &TaskInput, sha: &str, main: &str, base: &str, conflicts: &[String]) -> ChoreWork {
    ChoreWork {
        kind: "sync_main".into(),
        prompt: task.prompt.trim().to_string(),
        title: format!("Sync main into {base}"),
        red_sha: None,
        fixes_item: None,
        extra_slot: false,
        tier: Some(task.tier.item_tier().to_string()),
        sync: Some(SyncWork {
            sha: sha.to_string(),
            main: main.to_string(),
            conflicts: if conflicts.is_empty() {
                String::new()
            } else {
                format!("Merging {main} into {base} conflicts in: {}", conflicts.join(", "))
            },
        }),
    }
}

/// `syncBranch`: the sync's branch, named after main's commit.
pub fn sync_branch(sha: &str) -> String {
    crate::main_sync::sync_branch_name(sha)
}

impl ChoreWork {
    pub fn kind(&self) -> Option<ChoreKind> {
        kind_of(&self.kind)
    }

    /// The item the flow runs: this task's id, kept as a store item would be (`slice` 1; a main sync's branch is its own).
    pub fn item(&self, id: &str) -> Item {
        Item {
            id: id.to_string(),
            title: self.title.clone(),
            status: ItemStatus::Claimed,
            slice: 1,
            kind: Some(self.kind.clone()),
            milestone: Some(MILESTONE.to_string()),
            tier: self.tier.clone(),
            red_sha: self.red_sha.clone(),
            remaining: Some(self.sync.as_ref().map(|s| s.conflicts.clone()).unwrap_or_default()),
            branch: self.sync.as_ref().map(|s| sync_branch(&s.sha)),
            ..Item::default()
        }
    }

    /// The chore as the prompt's builder reads it: the store's words are the session's instructions.
    pub fn chore(&self) -> Option<Chore> {
        let kind = self.kind()?;
        let fallback =
            (kind == ChoreKind::PlaytestFix).then_some("Fix the problem a person found while trying the change out.");
        Some(Chore {
            kind,
            recent: Vec::new(),
            report: String::new(),
            sync_sha: self.sync.as_ref().map(|s| s.sha.clone()).unwrap_or_default(),
            main: self
                .sync
                .as_ref()
                .map_or_else(|| "main".to_string(), |s| s.main.clone()),
            conflicts: self.sync.as_ref().map(|s| s.conflicts.clone()).unwrap_or_default(),
            instructions: if self.prompt.is_empty() {
                fallback.unwrap_or_default().to_string()
            } else {
                self.prompt.clone()
            },
        })
    }

    /// The piece the prompt is filled from: the chore is the brief.
    pub fn piece(&self) -> RunPiece {
        serde_json::from_value(json!({ "title": self.title, "what": self.title })).unwrap_or_else(|_| {
            // A title is always text, so this does not happen; the brief stays empty rather than failing a chore.
            RunPiece {
                title: self.title.clone(),
                what: self.title.clone(),
                done_when: String::new(),
                attempts: Vec::new(),
                branch: None,
                change: None,
                approved_sha: None,
                conflict_of: None,
                held: None,
                other: Default::default(),
            }
        })
    }
}

/// The chore kind of a local chore item (`kind` set by [`ChoreWork::item`], in the `chores` milestone).
pub fn item_kind(item: &Item) -> Option<ChoreKind> {
    if item.milestone.as_deref() != Some(MILESTONE) {
        return None;
    }
    kind_of(item.kind.as_deref()?)
}

/// What a chore item does differently from a roadmap item's chain (TypeScript: `stepsCi.ts:291` and `:316`, `steps.ts:499`
/// and `:532`, `merge.ts:142` and `:285`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Flow {
    /// The checks that fail on the base branch too are this chore's to fix, so no baseline comparison forgives them
    /// (`fix_main`).
    pub no_baseline: bool,
    /// The code was reviewed on main already: no code gates, no review, no `ci-ready` label, merged with a merge commit
    /// (`sync_main`).
    pub merge_sync: bool,
}

impl Flow {
    pub fn of(item: &Item) -> Self {
        match item_kind(item) {
            Some(ChoreKind::FixMain) => Self {
                no_baseline: true,
                merge_sync: false,
            },
            Some(ChoreKind::SyncMain) => Self {
                no_baseline: false,
                merge_sync: true,
            },
            _ => Self::default(),
        }
    }
}

#[cfg(test)]
mod tests;
