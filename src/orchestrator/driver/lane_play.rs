//! **Play**, **Stop** and **Restart** in an item's lane (`play { lane: k }`, agent/04-architecture.md "Play"). The lane must hold an
//! item (the one that last used it here) with no session or step running in it, and nobody holding it now. While the
//! game runs the lane is held as a [`Holder::Play`], so nothing else starts there; it goes back when the game closes
//! or Stop ends it. A Restart of a lane with a game stops it and remembers the lane in `restarting`; once the lane is given back
//! a later poll plays it again, and if the game hasn't closed within [`crate::play::STOP_LIMIT`] the restart is dropped
//! (while paused or stopping it only stops). A lane that's empty, busy building, or past `lanes` is refused (logged: the ack went out already).
use super::{Links, ProjectLoop, SessionStarter};
use crate::lane_pool::{Holder, LanePool};
use crate::lanes::lane_path;
use crate::telemetry::{Event, event};
use crate::view::ItemCommand;
use std::path::Path;

/// A game running in an item's lane.
pub trait LanePlay: Send {
    /// Whether the game has closed.
    fn finished(&self) -> bool;
    /// Ends the game; it is [`finished`](Self::finished) once it is gone.
    fn stop(&self);
}

impl LanePlay for crate::play::LaneGame {
    fn finished(&self) -> bool {
        self.finished()
    }
    fn stop(&self) {
        self.stop()
    }
}

impl ProjectLoop {
    /// Every poll, paused or not: gives back the lanes whose game closed, and carries out the queued Play and Stop.
    /// A Play while `paused` (or stopping) is dropped, so nothing starts that nobody expects; a Stop always works.
    pub(super) fn carry_out_lane_commands(
        &mut self,
        pool: &mut LanePool,
        starter: &dyn SessionStarter,
        links: &Links,
        hold: bool,
    ) {
        self.release_finished_games(pool);
        self.finish_restarts(pool, starter, hold);
        for command in links.item_commands.take_lane(&self.id) {
            match command {
                ItemCommand::Play { lane, .. } if !hold => self.play_lane(pool, lane, starter),
                ItemCommand::Stop { lane, .. } => self.stop_lane(pool, lane),
                ItemCommand::RestartPlay { lane, .. } if !hold => self.restart_lane(pool, lane, starter),
                ItemCommand::RestartPlay { lane, .. } => {
                    self.stop_lane(pool, lane);
                    tracing::info!(
                        "{}: stopped lane {lane} for a restart but won't start it while paused or stopping",
                        self.id
                    );
                }
                _ => tracing::info!("{}: dropped a play while paused or stopping", self.id),
            }
        }
    }

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

    /// Restarts lane `lane`: a Play if no game runs there, else stops the game and plays again once it has closed.
    fn restart_lane(&mut self, pool: &mut LanePool, lane: u32, starter: &dyn SessionStarter) {
        if !self.playing.contains_key(&lane) {
            return self.play_lane(pool, lane, starter);
        }
        self.stop_lane(pool, lane);
        self.restarting.insert(lane, std::time::Instant::now());
    }

    /// Plays again the lanes whose game was stopped for a restart and has closed; drops one that is still open after
    /// [`crate::play::STOP_LIMIT`], or any while `hold` (paused or stopping).
    fn finish_restarts(&mut self, pool: &mut LanePool, starter: &dyn SessionStarter, hold: bool) {
        for (lane, since) in std::mem::take(&mut self.restarting) {
            if hold {
                tracing::info!(
                    "{}: dropped the restart of lane {lane} while paused or stopping",
                    self.id
                );
            } else if !self.playing.contains_key(&lane) {
                self.play_lane(pool, lane, starter);
            } else if since.elapsed() >= crate::play::STOP_LIMIT {
                tracing::info!("{}: dropped the restart of lane {lane}: the game didn't close", self.id);
            } else {
                self.restarting.insert(lane, since);
            }
        }
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
