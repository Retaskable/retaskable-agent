//! Which of the computer's lanes is free, and who holds each (docs/specs/agent/04-architecture.md section 4.1;
//! Retaskable.com agent/src/orchestratorSchedule.ts `freeLane`, `claimLane`, `laneForMerge` and `laneLimit`). The
//! computer's `lanes` are shared by all its projects, so one `LanePool` serves them all: a lane another project
//! holds is not free. Only running work holds a lane (an item's implementer, checks or review, or a Play), so the
//! caller takes one as that starts and gives it back as it stops; `claim` picks and takes in one step, so two
//! starts can never land in the same lane. Also here: each lane's disk use, for the snapshot's lanes list.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use tokio::sync::watch;

/// `laneScaling` (section 4.1): all `lanes` take new work only while less than `below_used_pct` of the 5-hour
/// window is used; above that, only the first `base` lanes do.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneScaling {
    pub base: u32,
    pub below_used_pct: f64,
}

/// The computer's settings that decide which lane is free: how many there are, the chore lane, lane scaling, and
/// which lanes run on another engine than Claude Code (Cursor, pi or Codex: those aren't held back by lane scaling, since the 5-hour window is Claude Code's).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PoolRules {
    pub lanes: u32,
    pub chore_lane: Option<u32>,
    pub scaling: Option<LaneScaling>,
    pub other_engine_lanes: BTreeSet<u32>,
}

/// What holds a lane: an item's running work, or a Play of the build in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Holder {
    Item {
        project: String,
        item: String,
    },
    Play {
        project: String,
        item: String,
    },
    /// A run on a piece (docs/specs/pieces.md section 3.1).
    Run {
        project: String,
        piece: String,
    },
    /// The project's trial plan session (first-run-check.md section 3.3).
    Trial {
        project: String,
    },
}

impl Holder {
    pub fn project(&self) -> &str {
        match self {
            Holder::Item { project, .. }
            | Holder::Play { project, .. }
            | Holder::Run { project, .. }
            | Holder::Trial { project } => project,
        }
    }

    fn is_item(&self, project: &str, item: &str) -> bool {
        matches!(self, Holder::Item { project: p, item: i } if p == project && i == item)
    }
}

/// An item asking for a lane: a chore asks for the chore lane, and `last_lane` is the one whose build is warm for it.
/// `fix_queued`: a playtest fix waits in its project, so a roadmap item never falls back on the chore lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub project: String,
    pub item: String,
    pub chore: bool,
    pub last_lane: Option<u32>,
    pub fix_queued: bool,
}

/// The computer's lanes and who holds each.
#[derive(Debug, Default, Clone)]
pub struct LanePool {
    rules: PoolRules,
    /// How much of the 5-hour window is used, in percent; `None` until it's known (then every lane takes work).
    used_pct: Option<f64>,
    held: BTreeMap<u32, Holder>,
    /// The lane each piece's last run took here, by project and piece: in memory only, never sent or saved.
    run_lanes: BTreeMap<(String, String), u32>,
}

impl LanePool {
    pub fn new(rules: PoolRules) -> Self {
        Self {
            rules,
            ..Self::default()
        }
    }

    pub fn rules(&self) -> &PoolRules {
        &self.rules
    }

    /// New settings. A lane past a lowered `lanes` keeps its holder: work there finishes, then it's left empty.
    pub fn set_rules(&mut self, rules: PoolRules) {
        self.rules = rules;
    }

    pub fn set_used_pct(&mut self, used_pct: Option<f64>) {
        self.used_pct = used_pct;
    }

    pub fn holder(&self, lane: u32) -> Option<&Holder> {
        self.held.get(&lane)
    }

    /// The lane `project`'s `item` holds, if any.
    pub fn lane_of(&self, project: &str, item: &str) -> Option<u32> {
        self.held
            .iter()
            .find(|(_, h)| h.is_item(project, item))
            .map(|(k, _)| *k)
    }

    /// Every lane currently held, for the tick's own telemetry.
    pub fn held_lanes(&self) -> impl Iterator<Item = u32> + '_ {
        self.held.keys().copied()
    }

    /// `laneLimit`: how many lanes may take new work: every one while the window is under lane scaling's threshold
    /// (or its use isn't known yet), otherwise only the base ones, never more than `lanes`.
    pub fn limit(&self) -> u32 {
        match (self.rules.scaling, self.used_pct) {
            (Some(s), Some(used)) if used >= s.below_used_pct => s.base.min(self.rules.lanes),
            _ => self.rules.lanes,
        }
    }

    /// Whether lane scaling keeps lane `k` from new work right now (the settings page and snapshot say so).
    pub fn scaled_out(&self, k: u32) -> bool {
        self.rules.scaling.is_some() && k > self.limit() && !self.rules.other_engine_lanes.contains(&k)
    }

    /// `freeLane`: a free lane, `prefer` if it's free (an item's last lane), otherwise the first. With a chore lane,
    /// `side` asks for that lane and otherwise it's never offered; without one every lane is the same.
    /// `may` leaves out lanes whose own engine can't start now.
    pub fn free_lane(&self, side: bool, prefer: Option<u32>, may: impl Fn(u32) -> bool) -> Option<u32> {
        let r = &self.rules;
        let chore = r.chore_lane.filter(|c| (1..=r.lanes).contains(c));
        let open = |k: u32| !self.held.contains_key(&k) && may(k);
        let limit = self.limit();
        let exempt = |k: u32| r.other_engine_lanes.contains(&k);
        let busy = self.held.keys().filter(|k| !exempt(**k)).count() as u32;
        let full = busy >= limit;
        if full && r.other_engine_lanes.is_empty() {
            return None;
        }
        let allowed = |k: u32| exempt(k) || (!full && k <= limit);
        if let (Some(c), true) = (chore, side) {
            return (allowed(c) && open(c)).then_some(c);
        }
        if let Some(p) = prefer
            && p <= r.lanes
            && allowed(p)
            && Some(p) != chore
            && open(p)
        {
            return Some(p);
        }
        (1..=r.lanes).find(|&k| Some(k) != chore && allowed(k) && open(k))
    }

    /// `claimLane`: the lane `want` should take next. A chore asks for the chore lane; a roadmap item takes its last
    /// lane or any other, and the chore lane only when nothing else is free and no playtest fix waits for it.
    pub fn claim_lane(&self, want: &Want) -> Option<u32> {
        self.claim_lane_where(want, |_| true)
    }

    /// `claim_lane`, leaving out lanes `may` refuses (those whose own engine can't start now), the chore-lane
    /// fallback included.
    pub fn claim_lane_where(&self, want: &Want, may: impl Fn(u32) -> bool) -> Option<u32> {
        self.free_lane(want.chore, want.last_lane, &may).or_else(|| {
            (!want.chore && !want.fix_queued)
                .then(|| self.free_lane(true, None, &may))
                .flatten()
        })
    }

    /// The `Want` of a run on `piece`: its last lane here when that is free, else any free lane. The chore lane
    /// follows `claim_lane`'s rules for an item.
    pub fn run_want(&self, project: &str, piece: &str) -> Want {
        Want {
            project: project.into(),
            item: piece.into(),
            chore: false,
            last_lane: self.run_lanes.get(&(project.into(), piece.into())).copied(),
            fix_queued: false,
        }
    }

    /// Picks a lane for a run on `piece` and holds it for the run, remembering it as the piece's last lane.
    pub fn claim_run(&mut self, project: &str, piece: &str) -> Option<u32> {
        self.claim_run_where(project, piece, |_| true)
    }

    /// `claim_run`, only in a lane `may` accepts.
    pub fn claim_run_where(&mut self, project: &str, piece: &str, may: impl Fn(u32) -> bool) -> Option<u32> {
        let k = self.claim_lane_where(&self.run_want(project, piece), may)?;
        let holder = Holder::Run {
            project: project.into(),
            piece: piece.into(),
        };
        self.held.insert(k, holder);
        self.run_lanes.insert((project.into(), piece.into()), k);
        Some(k)
    }

    /// Picks `want`'s lane and takes it, in one step.
    pub fn claim(&mut self, want: &Want) -> Option<u32> {
        self.claim_where(want, |_| true)
    }

    /// `claim`, only in a lane `may` accepts.
    pub fn claim_where(&mut self, want: &Want, may: impl Fn(u32) -> bool) -> Option<u32> {
        let k = self.claim_lane_where(want, may)?;
        self.held.insert(
            k,
            Holder::Item {
                project: want.project.clone(),
                item: want.item.clone(),
            },
        );
        Some(k)
    }

    /// Takes lane `k` for `holder` (a Play in an item's lane, or new work in a lane `free_lane` offered). False, and
    /// nothing changes, when it's out of range or someone holds it already: one holder per lane, always.
    pub fn take(&mut self, k: u32, holder: Holder) -> bool {
        if k == 0 || k > self.rules.lanes || self.held.contains_key(&k) {
            return false;
        }
        self.held.insert(k, holder);
        true
    }

    /// Gives lane `k` back, only if `holder` holds it. Whether it did.
    pub fn release(&mut self, k: u32, holder: &Holder) -> bool {
        if self.held.get(&k) == Some(holder) {
            self.held.remove(&k);
            return true;
        }
        false
    }
}

/// Rough disk use of a lane, in bytes: every file under it, `.git` and links (a sibling dependency's junction)
/// left out, so a lane isn't charged for the folder it points at. Walks the tree, so call it rarely.
pub fn dir_size(dir: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(entries) = fs::read_dir(&d) else { continue };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if entry.file_name() != ".git" {
                    stack.push(entry.path());
                }
            } else if kind.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}

/// How often each lane's disk use is measured (orchestratorSync.ts `LANE_SIZE_EVERY_MS`).
pub const SIZE_EVERY: std::time::Duration = std::time::Duration::from_secs(10 * 60);

/// Each project's lanes' disk use in GB, by project id then lane number, for its snapshot. Shared with the
/// connector, which sends a project's snapshot again when its sizes change.
#[derive(Debug, Clone)]
pub struct LaneSizes(Arc<watch::Sender<BTreeMap<String, BTreeMap<u32, f64>>>>);

impl Default for LaneSizes {
    fn default() -> Self {
        Self(Arc::new(watch::Sender::new(BTreeMap::new())))
    }
}

impl PartialEq for LaneSizes {
    fn eq(&self, other: &Self) -> bool {
        *self.0.borrow() == *other.0.borrow()
    }
}

impl LaneSizes {
    /// Lane `k`'s size for `project`, 0 until measured.
    pub fn gb(&self, project: &str, k: u32) -> f64 {
        self.0
            .borrow()
            .get(project)
            .and_then(|m| m.get(&k))
            .copied()
            .unwrap_or(0.0)
    }

    /// Records `project`'s sizes; listeners hear only of a real change.
    pub fn set(&self, project: &str, sizes: BTreeMap<u32, f64>) {
        self.0.send_if_modified(|all| {
            if all.get(project) == Some(&sizes) {
                return false;
            }
            all.insert(project.to_string(), sizes);
            true
        });
    }

    pub fn subscribe(&self) -> watch::Receiver<BTreeMap<String, BTreeMap<u32, f64>>> {
        self.0.subscribe()
    }

    /// Measures lanes 1..=`lanes` under `worktrees_dir` for `project`, a lane not on disk left out.
    pub fn measure(&self, project: &str, worktrees_dir: &Path, lanes: u32) {
        let sizes = (1..=lanes)
            .filter_map(|k| {
                let dir = crate::lanes::lane_path(worktrees_dir, k);
                dir.is_dir().then(|| (k, gb(dir_size(&dir))))
            })
            .collect();
        self.set(project, sizes);
    }
}

/// Bytes as GB, to two places, as the lanes panel shows them.
fn gb(bytes: u64) -> f64 {
    (bytes as f64 / 1e9 * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests;
