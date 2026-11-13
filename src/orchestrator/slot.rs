//! The slot `plan` and `look` runs share: one of them at a time on this computer (docs/specs/pieces.md section 3.2).
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The computer's one plan-or-look at a time: whoever holds the slot is the one running.
#[derive(Clone, Default)]
pub struct Slot(Arc<AtomicBool>);

/// Holds the [`Slot`] until it's dropped, however the run's task ends (a panic included).
pub struct SlotGuard(Arc<AtomicBool>);

impl Slot {
    pub fn busy(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// The slot, if nobody else has it.
    pub fn take(&self) -> Option<SlotGuard> {
        (!self.0.swap(true, Ordering::SeqCst)).then(|| SlotGuard(self.0.clone()))
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_run_holds_the_slot_at_a_time() {
        let slot = Slot::default();
        assert!(!slot.busy());
        let guard = slot.take().expect("a free slot is taken");
        assert!(slot.busy());
        assert!(slot.take().is_none(), "a held slot can't be taken again");
        assert!(slot.busy(), "a refused take leaves the holder in place");
        drop(guard);
        assert!(!slot.busy());
        assert!(slot.take().is_some());
    }

    #[test]
    fn clones_share_the_one_slot() {
        let slot = Slot::default();
        let other = slot.clone();
        let _guard = slot.take().unwrap();
        assert!(other.busy());
        assert!(other.take().is_none());
    }

    #[test]
    fn a_panicking_run_still_frees_the_slot() {
        let slot = Slot::default();
        let held = slot.clone();
        let result = std::panic::catch_unwind(move || {
            let _guard = held.take().unwrap();
            panic!("run blew up");
        });
        assert!(result.is_err());
        assert!(!slot.busy());
    }
}
