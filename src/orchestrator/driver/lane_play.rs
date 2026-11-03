//! **Play** and **Stop** in an item's lane (`play { lane: k }`, agent/04-architecture.md "Play"). The lane must hold an
//! item (the one that last used it here) with no session or step running in it, and nobody holding it now. While the
//! game runs the lane is held as a [`Holder::Play`], so nothing else starts there; it goes back when the game closes
//! or Stop ends it. A lane that's empty, busy building, or past `lanes` is refused (logged: the ack went out already).
use super::{ProjectLoop, SessionStarter};
use crate::lane_pool::{Holder, LanePool};
use crate::lanes::lane_path;
use crate::telemetry::{Event, event};
use std::path::Path;

impl ProjectLoop {
    /// Plays lane `lane`: takes it as a Play and runs `playCommand` in it.
    pub(super) fn play_lane(&mut self, pool: &mut LanePool, lane: u32, starter: &dyn SessionStarter) {
        let refuse = |why: &str| tracing::info!("{}: refused a play of lane {lane}: {why}", self.id);
        if lane == 0 || lane > pool.rules().lanes {
            return refuse("there is no such lane");
        }
        if self.playing.contains_key(&lane) {
            return refuse("a game is running there");
        }
        if pool.holder(lane).is_some() {
            return refuse("it is busy");
        }
        let command = self.project.play_command.trim();
        if command.is_empty() {
            return refuse("no play command is set");
        }
        let item = self
            .last_lane
            .iter()
            .find(|(id, k)| **k == lane && self.runs.contains_key(id.as_str()) && !self.running.busy(id))
            .map(|(id, _)| id.clone());
        let Some(item) = item else {
            return refuse("no item is idle in it");
        };
        let dir = lane_path(Path::new(&self.project.worktrees_dir), lane);
        let game = match starter.play_lane(&dir, command, lane) {
            Ok(game) => game,
            Err(e) => return refuse(&e),
        };
        let holder = Holder::Play {
            project: self.id.clone(),
            item: item.clone(),
        };
        assert!(pool.take(lane, holder), "the lane was free");
        self.events.add_in(
            &self.id,
            Event::LaneTaken(event::LaneTaken {
                item: Some(item.clone()),
                lane: Some(lane),
                ..Default::default()
            }),
        );
        self.playing.insert(lane, (item, game));
    }

    /// Ends the game in lane `lane`, and gives the lane back if it has closed.
    pub(super) fn stop_lane(&mut self, pool: &mut LanePool, lane: u32) {
        match self.playing.get(&lane) {
            Some((_, game)) => game.stop(),
            None => tracing::info!("{}: refused a stop of lane {lane}: no game is running there", self.id),
        }
        self.release_finished_games(pool);
    }

    /// Gives back every lane whose game has closed, to its item.
    pub(super) fn release_finished_games(&mut self, pool: &mut LanePool) {
        let done: Vec<u32> = (self.playing.iter())
            .filter(|(_, (_, game))| game.finished())
            .map(|(lane, _)| *lane)
            .collect();
        for lane in done {
            let Some((item, _)) = self.playing.remove(&lane) else {
                continue;
            };
            let holder = Holder::Play {
                project: self.id.clone(),
                item: item.clone(),
            };
            pool.release(lane, &holder);
            self.events.add_in(
                &self.id,
                Event::LaneFreed(event::LaneFreed {
                    item: Some(item),
                    lane: Some(lane),
                    why: Some("the game closed".into()),
                    ..Default::default()
                }),
            );
        }
    }
}
