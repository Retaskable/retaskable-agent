//! A leased item's merge as the loop sees it (`mergeQueueTurn`, `sendMergeResult`, `giveUpTurn`, `releaseMergeQueue` in the TS agent's
//! `mergeQueue.ts`, and `landed` and `sliceMerged` in `merge.ts` and `hostedWork.ts`). The store decides whose turn it is to merge into
//! the base: before the `merging` step runs the loop makes the change known to the queue (`merge_enqueue`), asks for the turn
//! (`merge_next`) and beats it (`merge_beat`, phase `merging`), and a store that can't be asked is a wait, never a way round (fail
//! closed). The step's end ([`MergeEnd`]) is the store's `merge_result`, kept to send again when it isn't taken. A change that
//! landed is completed (`complete`: built, remaining, merge commit, on-default-branch, the try-out), through the completion outbox,
//! which keeps the call across restarts and outages until the store answers (`outbox.rs`).
//!
//! A synthetic chore item has no lease: it asks for the turn as a chore entry, and every other store write of the chain (progress,
//! park, complete) is skipped for it.
use super::outbox::Sent;
use super::{Links, ProjectLoop};
use crate::backlog::complete::{Completion, OwedTryout, Playtest};
use crate::backlog::merge::{MergeEntry, MergeNext, MergeOutcome, MergePr};
use crate::drafts::land::{pr_number, workflow};
use crate::item_lifecycle::ItemState;
use crate::orchestrator::flow::answer;
use crate::orchestrator::steps::MergeEnd;

/// The turn this computer holds, for the item it was given for.
#[derive(Debug, Default)]
pub(super) struct Turns {
    turn: Option<(String, String)>,
    /// The turn was beaten in this pass, so the step may merge.
    ready: bool,
    /// Results the store couldn't take yet: the turn's token and what happened.
    pending: Vec<(String, MergeOutcome)>,
    opened: bool,
}

impl Turns {
    /// Whether this computer holds an item's merge turn (TS `held()`): the store would answer a piece's `merge_next` with it.
    pub(super) fn holds(&self) -> bool {
        self.turn.is_some()
    }

    /// Holds `item`'s turn `token`, as the store handed it out.
    #[cfg(test)]
    pub(super) fn hold(&mut self, item: &str, token: &str) {
        self.turn = Some((item.into(), token.into()));
    }

    /// Whether `item`'s merge step may run now.
    pub(super) fn may_merge(&self, item: &str) -> bool {
        self.ready && self.turn.as_ref().is_some_and(|(i, _)| i == item)
    }
}

impl ProjectLoop {
    /// Each pass, before the steps: results owed are sent, a turn this computer held across a restart is taken back, a turn whose
    /// item left merging is given up, and a `merging` item with no turn asks for one.
    pub(super) async fn merge_turns(&mut self, links: &Links) {
        self.turns.ready = false;
        // Nothing merges here and nothing is owed: no store call (a turn held from before a restart is asked for once an item merges).
        let merging = (self.runs.values()).any(|r| matches!(r.state, ItemState::Merging | ItemState::AwaitingMerge));
        if !merging && self.turns.pending.is_empty() && self.turns.turn.is_none() {
            return;
        }
        let (Some(repo), Ok(Some(token))) = (self.repo, links.store_token().await) else {
            return;
        };
        for (turn, outcome) in std::mem::take(&mut self.turns.pending) {
            if (self.client.merge_result(repo, Some(&token), &turn, &outcome).await).is_err() {
                self.turns.pending.push((turn, outcome));
            }
        }
        if !self.turns.opened {
            self.turns.opened = true;
            match self.client.merge_open(repo, Some(&token)).await {
                Ok(Some(t)) => {
                    let item = t.entry["item"].as_str().unwrap_or_default().to_string();
                    if self.runs.get(&item).is_some_and(|r| r.state == ItemState::Merging) {
                        self.turns.turn = Some((item, t.token));
                    } else {
                        let outcome = MergeOutcome::SentBack {
                            reason: "it no longer merges here".into(),
                        };
                        self.send_merge_result(repo, &token, t.token, outcome).await;
                    }
                }
                Ok(None) => {}
                Err(_) => self.turns.opened = false,
            }
        }
        self.release_turn_if_left(repo, &token).await;
        if self.turns.turn.is_none() {
            self.ask_turn(repo, &token).await;
        }
        // The beat just before the host call: only while the store says the turn is still ours does anything merge.
        if let Some((item, turn)) = self.turns.turn.clone()
            && self.runs.get(&item).is_some_and(|r| r.state == ItemState::Merging)
        {
            match self.client.merge_beat(repo, Some(&token), &turn, "merging").await {
                Ok(true) => self.turns.ready = true,
                Ok(false) => self.turns.turn = None,
                Err(e) => tracing::warn!(
                    "{}: the beat for {item}'s merge turn wasn't taken, so nothing merges: {e}",
                    self.id
                ),
            }
        }
    }

    /// Makes every `merging` item known to the queue and asks for the turn; one of this computer's items given it holds it.
    async fn ask_turn(&mut self, repo: u64, token: &str) {
        let waiting: Vec<String> = (self.runs.iter())
            .filter(|(id, r)| r.state == ItemState::Merging && !self.running.busy(id))
            .map(|(id, _)| id.clone())
            .collect();
        let base = self.project.base_branch.clone();
        let wf = workflow(&self.project);
        for id in &waiting {
            let record = self.records.get(id);
            let number = record
                .and_then(|r| r.flow.pr_number)
                .or_else(|| self.items.get(id)?.pr_url.as_deref().and_then(pr_number));
            let url = record
                .and_then(|r| r.flow.pr_url.clone())
                .or_else(|| self.items.get(id)?.pr_url.clone())
                .unwrap_or_default();
            let approved = record.and_then(|r| r.flow.approved_sha.clone());
            let Some(number) = number else { continue };
            let entry = MergeEntry {
                item: id,
                piece: None,
                base: &base,
                head: approved.as_deref().unwrap_or("unknown"),
                approved_sha: approved.as_deref(),
                pr: MergePr { number, url: &url },
                land: &wf.land,
                chore: !self.leases.contains_key(id),
                holds_branch: true,
            };
            if let Err(e) = self.client.merge_enqueue(repo, Some(token), &entry).await {
                self.store_error("merge_enqueue", &e);
                return;
            }
        }
        if waiting.is_empty() {
            return;
        }
        match self.client.merge_next(repo, Some(token), &base, None).await {
            Ok(MergeNext::Turn(t)) => {
                let item = t.entry["item"].as_str().unwrap_or_default().to_string();
                if waiting.contains(&item) {
                    self.turns.turn = Some((item, t.token));
                }
            }
            Ok(_) => {}
            Err(e) => self.store_error("merge_next", &e),
        }
    }

    /// The turn goes once its item no longer holds it here (merged, handed to a person, sent back, needs a person), so the next
    /// change's merge starts from the new base. A hand-off to auto-merge keeps it until the change lands.
    async fn release_turn_if_left(&mut self, repo: u64, token: &str) {
        let Some((item, turn)) = self.turns.turn.clone() else {
            return;
        };
        let state = self.runs.get(&item).map(|r| r.state);
        let handed_off = state == Some(ItemState::AwaitingMerge)
            && self
                .records
                .get(&item)
                .is_some_and(|r| r.flow.land.as_deref().is_some_and(|l| l != "person"));
        if matches!(state, Some(ItemState::Merging)) || handed_off {
            return;
        }
        self.turns.turn = None;
        let outcome = match state {
            Some(ItemState::NeedsHuman) => MergeOutcome::NeedsPerson {
                reason: "needs a person".into(),
            },
            _ => MergeOutcome::SentBack {
                reason: "it left merging".into(),
            },
        };
        self.send_merge_result(repo, token, turn, outcome).await;
    }

    async fn send_merge_result(&mut self, repo: u64, token: &str, turn: String, outcome: MergeOutcome) {
        if let Err(e) = self.client.merge_result(repo, Some(token), &turn, &outcome).await {
            tracing::info!(
                "{}: the {outcome:?} result for the merge turn wasn't taken, will send it again: {e}",
                self.id
            );
            self.turns.pending.push((turn, outcome));
        }
    }

    /// A merge step's end: the store hears it for the turn this item holds, and a change that landed is completed.
    pub(super) async fn merge_ended(&mut self, item: &str, end: MergeEnd, links: &Links) {
        let (Some(repo), Ok(Some(token))) = (self.repo, links.store_token().await) else {
            // Nothing can be told: the turn is let go of when the store answers (its lease on the turn runs out), the item's
            // state stands, and a landed change is completed once the lease is claimed back.
            if let MergeEnd::Merged { commit } = end {
                self.complete_landed(item, commit, links).await;
            }
            return;
        };
        let holds = self.turns.turn.as_ref().is_some_and(|(i, _)| i == item);
        let outcome = match &end {
            MergeEnd::Merged { commit } => Some(MergeOutcome::Merged {
                merge_commit: commit.clone(),
            }),
            MergeEnd::Updated { head } => Some(MergeOutcome::Updated {
                head: head.clone(),
                approved_intact: false,
            }),
            MergeEnd::HandedOff => Some(MergeOutcome::HandedOff),
            MergeEnd::Closed => Some(MergeOutcome::Closed),
            MergeEnd::SentBack { reason } => Some(MergeOutcome::SentBack { reason: reason.clone() }),
            MergeEnd::NeedsPerson { reason } => Some(MergeOutcome::NeedsPerson { reason: reason.clone() }),
        };
        if let (true, Some(outcome), Some((_, turn))) = (holds, outcome, self.turns.turn.clone()) {
            self.send_merge_result(repo, &token, turn, outcome).await;
            if !matches!(end, MergeEnd::HandedOff) {
                self.turns.turn = None;
            }
        }
        if let MergeEnd::Merged { commit } = end {
            self.complete_landed(item, commit, links).await;
        }
    }

    /// The slice merged: the store hears the outcome with what the implementer said is left (`sliceMerged`). Claimed back first when
    /// the item was parked waiting for the merge. The call is listed (and saved) before it is sent, so a store that doesn't answer
    /// keeps it for later passes and restarts. An item with no lease (a synthetic chore) has nothing to tell the store.
    async fn complete_landed(&mut self, item: &str, commit: Option<String>, links: &Links) {
        self.measure_item_merged(item, commit.as_deref());
        if !self.leases.contains_key(item) && self.parked.contains_key(item) {
            self.claim_back(links).await;
        }
        let (Some(repo), Some(lease)) = (self.repo, self.leases.get(item).cloned()) else {
            // No lease: nothing of this is the store's, the item is simply done here. A chore's task ends ok first
            // (`forget_finished_chores`).
            if self.chores.contains_key(item) {
                if let Some(run) = self.runs.get_mut(item) {
                    run.state = ItemState::Done;
                }
            } else {
                self.runs.remove(item);
                self.items.remove(item);
            }
            return;
        };
        // Listed first, with the deferred try-out in it, so a store that doesn't answer (or a restart) keeps both.
        let completion = self.landed_completion(item, commit.clone());
        let tryout = self.take_tryout(item, commit, links);
        if self.outbox.get(item).is_none_or(|p| p.lease != lease) {
            let since = self.now_secs();
            let pending = crate::backlog::complete::Pending {
                lease: lease.clone(),
                completion: completion.clone(),
                since,
                completed: false,
                tryout,
            };
            self.outbox.insert(item.to_string(), pending);
            self.save_leases();
        }
        let Ok(Some(token)) = links.store_token().await else {
            // Listed anyway: the loop sends it with the token it has when the store answers.
            return;
        };
        if let Some(run) = self.runs.get_mut(item) {
            run.merged_resolved(completion.built);
        }
        if let Sent::Kept(why) | Sent::Dropped(why) = self.complete_item(repo, &token, item, &lease, completion).await {
            tracing::info!(
                "{}: merged {item}, but the shared backlog didn't take it yet: {why}; sent again on the next pass",
                self.id
            );
        }
    }

    /// The try-out deferred until the merge (`pendingPlaytest`, read from the record, else from the item's details, as
    /// TypeScript reads it from the store), with what `tryout_add` needs and the session and lane that built the slice. Taken: the
    /// record and the details are cleared, the details in the same write as the completion.
    fn take_tryout(&mut self, item: &str, commit: Option<String>, links: &Links) -> Option<OwedTryout> {
        let record = self.records.get_mut(item);
        let kept = record.as_ref().and_then(|r| r.flow.pending_playtest.clone());
        let from_details = self.decisions.get(item).and_then(|d| d.now.pending_playtest.clone());
        let (session_id, lane, pr_url) = match &record {
            Some(r) => (r.impl_session_id.clone(), r.lane, r.flow.pr_url.clone()),
            None => (None, None, None),
        };
        if let Some(r) = record {
            r.flow.pending_playtest = None;
            r.flow.playtest = None;
        }
        if let Some(held) = self.decisions.get_mut(item) {
            held.now.set_tryouts(None, None);
        }
        let what = kept.or(from_details)?;
        let found = self.items.get(item);
        let served_at = (self.project.deploy.staging.as_ref())
            .and_then(|t| t.url.as_deref())
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(String::from);
        Some(OwedTryout {
            merged_at: crate::text::now_iso8601(),
            title: found
                .map(|i| i.title.clone())
                .filter(|t| !t.trim().is_empty())
                .unwrap_or_else(|| item.to_string()),
            what: what.what,
            steps: what.steps,
            pr_url: pr_url.or_else(|| found.and_then(|i| i.pr_url.clone())),
            sha: commit,
            served_at,
            machine: links.machine.clone(),
            session_id,
            lane,
        })
    }

    fn landed_completion(&self, item: &str, commit: Option<String>) -> Completion {
        let said = self
            .data_dir
            .as_deref()
            .and_then(|dir| answer::load(dir, &self.project.id_or_derived(), item));
        let built = said.as_ref().and_then(|a| a.item_done) == Some(true) && commit.is_some();
        let remaining = said
            .as_ref()
            .and_then(|a| a.remaining.clone())
            .or_else(|| self.items.get(item).and_then(|i| i.remaining.clone()))
            .unwrap_or_default();
        Completion {
            built,
            merge_commit: commit.filter(|_| built),
            on_default_branch: built && self.seen.default_branch.as_deref() == Some(self.project.base_branch.as_str()),
            remaining: Some(remaining),
            playtest: said.and_then(|a| a.playtest).map(|p| Playtest {
                what: p.what,
                steps: p.steps,
            }),
        }
    }
}
