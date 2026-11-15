//! One item's decision facts as the loop holds them: what the store last said (`known`, with its version) and what the
//! agent now believes (`now`). The agent edits `now`; [`ItemDecisions::flush`] sends the difference as one
//! `details_report` patch. A flush the store didn't take leaves `now` ahead of `known`, which is the whole outbox: the
//! next flush sends the difference again. Nothing is kept on disk.
use super::Decisions;
use crate::backlog::complete::{DetailsRecord, DetailsReport};
use crate::backlog::{BacklogError, Client};

/// How many times a patch is sent again after the details moved under it.
const CONFLICT_TRIES: u32 = 3;

/// The store's record of an item's decisions: its version and the fields.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Synced {
    pub version: u64,
    pub decisions: Decisions,
}

impl Synced {
    fn from_record(record: &DetailsRecord) -> Self {
        Self {
            version: record.version,
            decisions: Decisions::from_details(&record.details),
        }
    }
}

/// An item's decisions: `now` is what the loop reads and edits.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ItemDecisions {
    known: Synced,
    pub now: Decisions,
}

impl ItemDecisions {
    /// Reads the item's record from the store (a fresh item has none: nothing set, version 0).
    pub async fn load(client: &Client, repo: u64, token: Option<&str>, id: &str) -> Result<Self, BacklogError> {
        let known = match client.details_get(repo, token, id).await? {
            Some(record) => Synced::from_record(&record),
            None => Synced::default(),
        };
        Ok(Self {
            now: known.decisions.clone(),
            known,
        })
    }

    /// Takes a record the store pushed (`details_changed`) as the new known state. Edits not yet sent stay on top of it:
    /// a field this side changed keeps its value, every other field follows the store.
    pub fn refresh(&mut self, record: &DetailsRecord) {
        if record.version <= self.known.version {
            return;
        }
        let mine = self.now.diff(&self.known.decisions);
        let next = Synced::from_record(record);
        let mut now = next.decisions.clone();
        now.apply(&mine);
        self.known = next;
        self.now = now;
    }

    /// Whether `now` has something the store hasn't been told.
    pub fn dirty(&self) -> bool {
        self.now != self.known.decisions
    }

    /// Sends what changed. Sent again with the store's new version if the details moved, up to three times; any other
    /// failure leaves the difference to the next flush. `lease` is the item's lease token, none for a parked item's owner.
    pub async fn flush(
        &mut self,
        client: &Client,
        repo: u64,
        token: Option<&str>,
        lease: Option<&str>,
        id: &str,
    ) -> Result<(), BacklogError> {
        let patch = self.now.diff(&self.known.decisions);
        if patch.is_empty() {
            return Ok(());
        }
        let mut base = self.known.version;
        for attempt in 0..=CONFLICT_TRIES {
            let report = DetailsReport {
                id,
                lease,
                base_version: base,
                patch: patch.clone(),
                note: None,
            };
            match client.details_report(repo, token, &report).await {
                Ok(record) => {
                    // The record is the store's truth: fields others changed come with it, ours are in it.
                    self.known = Synced::from_record(&record);
                    self.now = self.known.decisions.clone();
                    return Ok(());
                }
                Err(e) if e.code() == Some("conflict") && attempt < CONFLICT_TRIES => {
                    let Some(record) = client.details_get(repo, token, id).await? else {
                        return Err(e);
                    };
                    base = record.version;
                    self.known = Synced::from_record(&record);
                    // Our edits stay on top of the fresh record.
                    let mut now = self.known.decisions.clone();
                    now.apply(&patch);
                    self.now = now;
                }
                Err(e) => return Err(e),
            }
        }
        unreachable!("the last attempt returns")
    }
}

#[cfg(test)]
mod tests;
