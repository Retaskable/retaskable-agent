//! Per-model session caps (`modelConcurrency`, docs/specs/agent/07.1-model-tiers.md): only `opus` and `sonnet` can be
//! capped; `haiku`, `local` and any other model never wait.
use std::sync::{Arc, OnceLock};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::ModelConcurrency;

/// The slots for `opus` and `sonnet`; `None` is no cap.
#[derive(Clone, Default)]
pub struct ModelSlots {
    opus: Option<Arc<Semaphore>>,
    sonnet: Option<Arc<Semaphore>>,
}

/// Holds one slot until it's dropped, however the session's task ends (a panic included).
pub struct ModelSlot(#[allow(dead_code)] Option<OwnedSemaphorePermit>);

impl ModelSlots {
    pub fn new(caps: ModelConcurrency) -> Self {
        let slots = |cap: Option<u32>| cap.map(|n| Arc::new(Semaphore::new(n as usize)));
        Self {
            opus: slots(caps.opus),
            sonnet: slots(caps.sonnet),
        }
    }

    /// Whether `model` has a slot free right now, without taking it. A model with no cap of its own always has room.
    pub fn has_room(&self, model: &str) -> bool {
        self.has_room_after(model, 0)
    }

    /// Whether `model` would still have a slot free after `pending` more sessions took theirs.
    pub fn has_room_after(&self, model: &str, pending: u32) -> bool {
        let sem = match model {
            "opus" => &self.opus,
            "sonnet" => &self.sonnet,
            _ => &None,
        };
        sem.as_ref().is_none_or(|s| s.available_permits() > pending as usize)
    }

    /// Waits until `model` has room. A model with no cap of its own is never held.
    pub async fn acquire(&self, model: &str) -> ModelSlot {
        let sem = match model {
            "opus" => &self.opus,
            "sonnet" => &self.sonnet,
            _ => &None,
        };
        match sem {
            // The semaphore is never closed, so this can't fail.
            Some(sem) => ModelSlot(sem.clone().acquire_owned().await.ok()),
            None => ModelSlot(None),
        }
    }
}

static SLOTS: OnceLock<ModelSlots> = OnceLock::new();

/// Hands over the process's one set of slots, built from `cfg.machine.model_concurrency`.
pub fn set_global(slots: ModelSlots) {
    let _ = SLOTS.set(slots);
}

/// The process's slots, once handed over.
pub fn global() -> Option<&'static ModelSlots> {
    SLOTS.get()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn slots(opus: Option<u32>, sonnet: Option<u32>) -> ModelSlots {
        ModelSlots::new(ModelConcurrency { opus, sonnet })
    }

    async fn waits(s: &ModelSlots, model: &str) -> bool {
        tokio::time::timeout(Duration::from_millis(50), s.acquire(model))
            .await
            .is_err()
    }

    #[tokio::test]
    async fn a_second_opus_waits_for_the_first_guard_to_drop() {
        let s = slots(Some(1), None);
        let first = s.acquire("opus").await;
        assert!(waits(&s, "opus").await);
        drop(first);
        assert!(!waits(&s, "opus").await);
    }

    #[tokio::test]
    async fn a_full_opus_cap_holds_no_other_model() {
        let s = slots(Some(1), None);
        let _opus = s.acquire("opus").await;
        for model in ["sonnet", "haiku", "local"] {
            assert!(!waits(&s, model).await, "{model}");
        }
    }

    #[tokio::test]
    async fn has_room_follows_the_slots_without_taking_one() {
        let s = slots(Some(1), None);
        assert!(s.has_room("opus"));
        let first = s.acquire("opus").await;
        assert!(!s.has_room("opus"));
        assert!(s.has_room("sonnet") && s.has_room("haiku"));
        drop(first);
        assert!(s.has_room("opus"));
    }

    #[tokio::test]
    async fn no_cap_never_waits() {
        let s = slots(None, None);
        let _held = (
            s.acquire("opus").await,
            s.acquire("opus").await,
            s.acquire("sonnet").await,
        );
        assert!(!waits(&s, "opus").await);
    }

    #[tokio::test]
    async fn a_panicking_task_frees_its_slot() {
        let s = slots(None, Some(1));
        let s2 = s.clone();
        let task = tokio::spawn(async move {
            let _slot = s2.acquire("sonnet").await;
            panic!("session blew up");
        });
        assert!(task.await.is_err());
        assert!(!waits(&s, "sonnet").await);
    }
}
