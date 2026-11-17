//! **Play this build** and its **Stop** (`play { itemId }`, `stop_play { itemId }`; `playItem`, `playBuild`, `stopItem` in the TS
//! agent's `lanePlay.ts`): an item's build played by the person who wants to try it. The commands are queued
//! ([`crate::view::ItemCommand::PlayItem`]) and carried out here with the lane commands, paused or not for a Stop, and answered
//! when the game has started or why not. A command does the thing asked and starts nothing else.
//!
//! - An item whose lane is free here and that last used it plays **there** (`try_play_lane`); an item that still holds a lane
//!   is being worked on ("Something is building in lane k; try again when it's idle.").
//! - Any other item held here with a pushed branch plays from the project's **play checkout** (`<worktreesDir>/play`,
//!   [`crate::lanes::prepare_play_checkout`]): its branch's tip, detached, so it can be tried while every lane is busy and never
//!   holds up the item's own steps. The checkout is made on its own thread (git waits), the game started on the next poll. One
//!   build plays there at a time: another's is stopped first (up to [`crate::play::STOP_LIMIT`]), and this item's, already open,
//!   is left as it is. A step running for the item refuses it ("A step is running for this item; try again in a moment.").
//! - An item this computer doesn't hold is refused: "Item <id> isn't waiting for a try-out on its branch." Playing another
//!   computer's try-out from its row in the store's queue (`visit`, `onBranch`) isn't done: this agent writes no on-branch
//!   try-out rows, and a computer that holds the item is the one to play it.
//!
//! Not done, as in TS only for a project that isn't Retaskable itself: the TS agent refuses Play this build for a project that
//! builds Retaskable (`selfHosted`); this agent has no such project. A paused computer starts no build (the lane Play is
//! dropped the same way); a Stop always works.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::json;

use super::lane_play::{LanePlay, Launch};
use super::{Links, ProjectLoop};
use crate::lane_pool::LanePool;

/// What TS says when there is no way to play.
const NO_COMMAND: &str = "No play command set. Add one in Settings → Project settings, under How to try it.";

/// A build in the play checkout, and the one waiting to take its place.
#[derive(Default)]
pub(super) struct BuildPlay {
    /// The item whose game runs in the play checkout.
    playing: Option<(String, Box<dyn LanePlay>)>,
    next: Option<Next>,
}

enum Next {
    /// The build that was playing is closing, so this one can have the checkout.
    Stopping {
        item: String,
        branch: String,
        command_id: String,
        since: Instant,
    },
    /// The checkout is being made on its own thread.
    Preparing {
        item: String,
        branch: String,
        command_id: String,
        done: Arc<Mutex<Option<Result<PathBuf, String>>>>,
    },
}

impl ProjectLoop {
    fn answer_command(&self, links: &Links, command_id: &str, why: Option<String>) {
        let msg = match why {
            None => json!({ "type": "ack", "id": command_id }),
            Some(reason) => json!({ "type": "nack", "id": command_id, "reason": reason }),
        };
        links.item_commands.answer(&self.id, msg);
    }

    /// Each poll: forgets a build whose game closed, and moves the one waiting along (starts its checkout once the other build
    /// has closed, starts its game once the checkout is made).
    pub(super) fn finish_build_play(&mut self, launch: Launch<'_>, links: &Links) {
        if self.build_play.playing.as_ref().is_some_and(|(_, g)| g.finished()) {
            self.build_play.playing = None;
        }
        match self.build_play.next.take() {
            None => {}
            Some(Next::Stopping {
                item,
                branch,
                command_id,
                since,
            }) => {
                if self.build_play.playing.is_some() && since.elapsed() >= crate::play::STOP_LIMIT {
                    let why = "The build that was playing didn't close in time, so this one wasn't started.";
                    self.answer_command(links, &command_id, Some(why.into()));
                } else if self.build_play.playing.is_some() {
                    self.build_play.next = Some(Next::Stopping {
                        item,
                        branch,
                        command_id,
                        since,
                    });
                } else {
                    self.prepare_build(item, branch, command_id);
                }
            }
            Some(Next::Preparing {
                item,
                branch,
                command_id,
                done,
            }) => {
                let ready = done.lock().unwrap_or_else(|e| e.into_inner()).take();
                match ready {
                    None => {
                        self.build_play.next = Some(Next::Preparing {
                            item,
                            branch,
                            command_id,
                            done,
                        });
                    }
                    Some(Err(e)) => {
                        self.answer_command(links, &command_id, Some(format!("Couldn't check out {branch}: {e}")));
                    }
                    Some(Ok(dir)) => {
                        let command = self.project.play_command.trim().to_string();
                        match launch(&dir, &command, 0) {
                            Ok(game) => {
                                tracing::info!("{}: playing {item}'s build from the play checkout", self.id);
                                self.build_play.playing = Some((item, game));
                                self.answer_command(links, &command_id, None);
                            }
                            Err(e) => self.answer_command(links, &command_id, Some(e)),
                        }
                    }
                }
            }
        }
    }

    /// Makes the play checkout on its own thread; the game is started when a poll finds it ready.
    fn prepare_build(&mut self, item: String, branch: String, command_id: String) {
        let done = Arc::new(Mutex::new(None));
        let (slot, repo, worktrees, on) = (
            done.clone(),
            PathBuf::from(&self.project.repo_path),
            PathBuf::from(&self.project.worktrees_dir),
            branch.clone(),
        );
        std::thread::spawn(move || {
            let made = crate::lanes::prepare_play_checkout(&repo, &worktrees, &on).map_err(|e| format!("{e:#}"));
            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(made);
        });
        self.build_play.next = Some(Next::Preparing {
            item,
            branch,
            command_id,
            done,
        });
    }

    /// **Play this build** of `item` (see the module's header).
    pub(super) fn play_item(
        &mut self,
        pool: &mut LanePool,
        item: &str,
        command_id: &str,
        launch: Launch<'_>,
        links: &Links,
        hold: bool,
    ) {
        let refuse = |me: &Self, why: String| me.answer_command(links, command_id, Some(why));
        if hold {
            return refuse(self, "This computer is paused, so it didn't start a build.".into());
        }
        if self.project.play_command.trim().is_empty() {
            return refuse(self, NO_COMMAND.into());
        }
        if self
            .build_play
            .playing
            .as_ref()
            .is_some_and(|(i, g)| i == item && !g.finished())
        {
            return self.answer_command(links, command_id, None);
        }
        if let Some(lane) = pool.lane_of(&self.id, item) {
            return refuse(
                self,
                format!("Something is building in lane {lane}; try again when it's idle."),
            );
        }
        // The lane it last used, free, plays there.
        let free_lane = (self.last_lane.get(item).copied())
            .filter(|k| self.runs.contains_key(item) && pool.holder(*k).is_none() && !self.playing.contains_key(k));
        if let Some(lane) = free_lane {
            let done = self
                .try_play_lane(pool, lane, launch)
                .err()
                .map(|why| format!("Lane {lane}: {why}."));
            return self.answer_command(links, command_id, done);
        }
        if !self.runs.contains_key(item) {
            return refuse(self, format!("Item {item} isn't waiting for a try-out on its branch."));
        }
        if self.running.busy(item) {
            return refuse(self, "A step is running for this item; try again in a moment.".into());
        }
        let branch = (self.records.get(item))
            .and_then(|r| r.branch.clone())
            .or_else(|| self.items.get(item).and_then(|i| i.branch.clone()))
            .filter(|b| !b.is_empty());
        let Some(branch) = branch else {
            return refuse(self, format!("Item {item} has no build to play yet."));
        };
        if self.build_play.next.is_some() {
            return refuse(
                self,
                "A build is being checked out to play; try again in a moment.".into(),
            );
        }
        // Another item's build open in the play checkout is closed first.
        if let Some((_, game)) = &self.build_play.playing {
            game.stop();
            self.build_play.next = Some(Next::Stopping {
                item: item.to_string(),
                branch,
                command_id: command_id.to_string(),
                since: Instant::now(),
            });
            return;
        }
        self.prepare_build(item.to_string(), branch, command_id.to_string());
    }

    /// **Stop** `item`'s build: its game in the play checkout, else its game in a lane. Acked whether or not one was running, as TS.
    pub(super) fn stop_item(&mut self, pool: &mut LanePool, item: &str, command_id: &str, links: &Links) {
        if let Some((playing, game)) = &self.build_play.playing
            && playing == item
        {
            game.stop();
        } else if let Some(lane) = (self.playing.iter()).find(|(_, (i, _))| i == item).map(|(k, _)| *k) {
            self.stop_lane(pool, lane);
        }
        self.answer_command(links, command_id, None);
    }
}
