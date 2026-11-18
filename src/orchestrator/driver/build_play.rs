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
//! - An item this computer doesn't hold is **visited** (`visit` in `lanePlay.ts`): another computer's slice, played from its
//!   on-branch row in the store's try-out queue (`tryouts_list`, a row with `onBranch`), at the row's approved commit, in the
//!   same play checkout, without taking the item over. No such row is "Item <id> isn't waiting for a try-out on its branch."
//!   The store is read on a later pass of the loop ([`ProjectLoop::carry_out_visits`], it is async); a store that can't be
//!   read refuses (fail closed).
//! - **Restart** (`restart_play { itemId }`, `restartItem`): an item whose game runs (in its lane or the play checkout) is
//!   stopped and played again once it has closed (up to [`crate::play::STOP_LIMIT`], else "It didn't stop in time, so it wasn't
//!   started again."); one with no game is just played. A Stop of the item cancels a pending restart.
//! - "Play started" goes on the item's timeline as an `item_state` event, and the build in the play checkout is the snapshot's
//!   `playingItem`.
//!
//! Not done, as in TS only for a project that isn't Retaskable itself: Play this build and its Restart are refused for a
//! project whose repo is the checkout this (development) build runs from ([`crate::play::self_hosted`], `selfHosted`). A paused
//! computer starts no build (the lane Play is dropped the same way); a Stop always works.
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde_json::json;

use super::lane_play::{LanePlay, Launch};
use super::{Links, ProjectLoop};
use crate::lane_pool::LanePool;
use crate::telemetry::{Event, event};

/// What TS says when there is no way to play.
const NO_COMMAND: &str = "No play command set. Add one in Settings → Project settings, under How to try it.";

/// A build in the play checkout, and the one waiting to take its place.
#[derive(Default)]
pub(super) struct BuildPlay {
    /// The item whose game runs in the play checkout.
    playing: Option<(String, Box<dyn LanePlay>)>,
    next: Option<Next>,
    /// Plays of items this computer doesn't hold, waiting for the store to be read.
    visits: Vec<(String, String)>,
    /// Restarts of items whose game was asked to close: `(item, command id, since)`.
    restarts: Vec<(String, String, Instant)>,
}

impl BuildPlay {
    /// A game runs in the play checkout, or one is being made ready (TS `playingBuild`, `preparingPlay`).
    pub(super) fn busy(&self) -> bool {
        self.playing.is_some() || self.next.is_some()
    }

    /// The item whose game runs in the play checkout, for the snapshot.
    pub(super) fn playing_item(&self) -> Option<String> {
        (self.playing.as_ref()).map(|(item, _)| item.clone())
    }
}

enum Next {
    /// The build that was playing is closing, so this one can have the checkout.
    Stopping {
        item: String,
        branch: String,
        sha: Option<String>,
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
                sha,
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
                        sha,
                        command_id,
                        since,
                    });
                } else {
                    self.prepare_build(item, branch, sha, command_id);
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
                                self.note_play_started(&item, None);
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
    fn prepare_build(&mut self, item: String, branch: String, sha: Option<String>, command_id: String) {
        let done = Arc::new(Mutex::new(None));
        let (slot, repo, worktrees, on) = (
            done.clone(),
            PathBuf::from(&self.project.repo_path),
            PathBuf::from(&self.project.worktrees_dir),
            branch.clone(),
        );
        std::thread::spawn(move || {
            let made = crate::lanes::prepare_play_checkout(&repo, &worktrees, &on, sha.as_deref())
                .map_err(|e| format!("{e:#}"));
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
            // Another computer's slice: its row in the store says where to find it (`carry_out_visits`).
            self.build_play.visits.push((item.to_string(), command_id.to_string()));
            return;
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
        self.begin_build(item, branch, None, command_id, links);
    }

    /// Starts the build of `item` on `branch` (at `sha`, when a try-out row names one) in the play checkout: closes another's game
    /// first, or makes the checkout now.
    fn begin_build(&mut self, item: &str, branch: String, sha: Option<String>, command_id: &str, links: &Links) {
        if self.build_play.next.is_some() {
            return self.answer_command(
                links,
                command_id,
                Some("A build is being checked out to play; try again in a moment.".into()),
            );
        }
        // Another item's build open in the play checkout is closed first.
        if let Some((_, game)) = &self.build_play.playing {
            game.stop();
            self.build_play.next = Some(Next::Stopping {
                item: item.to_string(),
                branch,
                sha,
                command_id: command_id.to_string(),
                since: Instant::now(),
            });
            return;
        }
        self.prepare_build(item.to_string(), branch, sha, command_id.to_string());
    }

    /// Each pass, with the store: plays the slices other computers hold that were asked for (`visit`). The store's try-out
    /// queue is read for the item's on-branch row; its branch and approved commit are what plays.
    pub(super) async fn carry_out_visits(&mut self, links: &Links, hold: bool) {
        for (item, command_id) in std::mem::take(&mut self.build_play.visits) {
            let refuse = |me: &Self, why: String| me.answer_command(links, &command_id, Some(why));
            if hold {
                refuse(self, "This computer is paused, so it didn't start a build.".into());
                continue;
            }
            let (Some(repo), Ok(Some(token))) = (self.repo, links.store_token().await) else {
                refuse(
                    self,
                    "Couldn't read the shared backlog's try-out queue: this computer isn't linked to it.".into(),
                );
                continue;
            };
            let list = match self.client.tryouts_list(repo, Some(&token)).await {
                Ok(list) => list,
                Err(e) => {
                    self.store_error("tryouts_list", &e);
                    refuse(self, format!("Couldn't read the shared backlog's try-out queue: {e}"));
                    continue;
                }
            };
            let row = (list.waiting.into_iter()).find(|t| t.item_id == item && t.on_branch.is_some());
            let Some(on) = row.and_then(|t| t.on_branch) else {
                refuse(self, format!("Item {item} isn't waiting for a try-out on its branch."));
                continue;
            };
            if (self.build_play.playing.as_ref()).is_some_and(|(i, g)| *i == item && !g.finished()) {
                self.answer_command(links, &command_id, None);
                continue;
            }
            self.begin_build(&item, on.branch, Some(on.sha), &command_id, links);
        }
    }

    /// Notes "Play started" on an item this computer holds (a visited slice is no item here, as in TS).
    pub(super) fn note_play_started(&mut self, item: &str, lane: Option<u32>) {
        let Some(run) = self.runs.get(item) else {
            return;
        };
        let state = crate::orchestrator::steps::state_name(run.state);
        self.events.add_in(
            &self.id,
            Event::ItemState(event::ItemState {
                item: Some(item.to_string()),
                from: Some(state.into()),
                to: Some(state.into()),
                note: Some(lane.map_or("Play started".to_string(), |k| format!("Play started in lane {k}"))),
                lane,
                ..Default::default()
            }),
        );
    }

    /// Whether `item`'s game is running, in its lane or in the play checkout.
    fn game_of(&self, item: &str) -> bool {
        (self.build_play.playing.as_ref()).is_some_and(|(i, g)| i == item && !g.finished())
            || self.playing.values().any(|(i, g)| i == item && !g.finished())
    }

    /// **Restart** `item`'s build (`restart_play { itemId }`): a game that runs is stopped, and played again by
    /// [`finish_item_restarts`](Self::finish_item_restarts) once it has closed; with none, it is just played.
    pub(super) fn restart_item(
        &mut self,
        pool: &mut LanePool,
        item: &str,
        command_id: &str,
        launch: Launch<'_>,
        links: &Links,
        hold: bool,
    ) {
        if !self.game_of(item) {
            return self.play_item(pool, item, command_id, launch, links, hold);
        }
        self.stop_game(pool, item);
        if hold {
            return self.answer_command(
                links,
                command_id,
                Some("This computer is paused, so it didn't start a build.".into()),
            );
        }
        self.build_play
            .restarts
            .push((item.to_string(), command_id.to_string(), Instant::now()));
    }

    /// Each poll: plays again the items whose game closed for a restart; drops one still open after
    /// [`crate::play::STOP_LIMIT`], or any while `hold` (paused or stopping).
    pub(super) fn finish_item_restarts(&mut self, pool: &mut LanePool, launch: Launch<'_>, links: &Links, hold: bool) {
        for (item, command_id, since) in std::mem::take(&mut self.build_play.restarts) {
            if self.game_of(&item) {
                if since.elapsed() >= crate::play::STOP_LIMIT {
                    let why = "It didn't stop in time, so it wasn't started again.";
                    self.answer_command(links, &command_id, Some(why.into()));
                } else {
                    self.build_play.restarts.push((item, command_id, since));
                }
            } else {
                self.play_item(pool, &item, &command_id, launch, links, hold);
            }
        }
    }

    /// Ends `item`'s game: in the play checkout, else in a lane.
    fn stop_game(&mut self, pool: &mut LanePool, item: &str) {
        if let Some((playing, game)) = &self.build_play.playing
            && playing == item
        {
            game.stop();
        } else if let Some(lane) = (self.playing.iter()).find(|(_, (i, _))| i == item).map(|(k, _)| *k) {
            self.stop_lane(pool, lane);
        }
    }

    /// **Stop** `item`'s build: its game in the play checkout, else its game in a lane. Acked whether or not one was running, as TS.
    /// A restart waiting for it is cancelled.
    pub(super) fn stop_item(&mut self, pool: &mut LanePool, item: &str, command_id: &str, links: &Links) {
        let waiting: Vec<_> = (self.build_play.restarts.iter())
            .filter(|(i, _, _)| i == item)
            .map(|(_, id, _)| id.clone())
            .collect();
        self.build_play.restarts.retain(|(i, _, _)| i != item);
        for restart in waiting {
            let why = "It was stopped, so it wasn't started again.";
            self.answer_command(links, &restart, Some(why.into()));
        }
        self.stop_game(pool, item);
        self.answer_command(links, command_id, None);
    }
}
