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
