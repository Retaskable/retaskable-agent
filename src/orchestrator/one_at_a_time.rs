//! The slot a computer's plan and look runs share: only one of them runs at a time (docs/specs/pieces.md section 3.2).
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// One slot; clones are the same slot.
#[derive(Debug, Clone, Default)]
pub struct OneAtATime(Arc<AtomicBool>);

impl OneAtATime {
    /// Takes the slot, or `None` when a run holds it. The slot is free again when the guard drops, however the run ends.
    pub fn take(&self) -> Option<Held> {
        (!self.0.swap(true, Ordering::SeqCst)).then(|| Held(self.0.clone()))
    }

    pub fn is_free(&self) -> bool {
        !self.0.load(Ordering::SeqCst)
    }
}

/// A run's hold on the slot.
#[derive(Debug)]
pub struct Held(Arc<AtomicBool>);

impl Drop for Held {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_holder_at_a_time_and_the_drop_frees_it() {
        let slot = OneAtATime::default();
        let held = slot.take().unwrap();
        assert!(!slot.is_free() && slot.clone().take().is_none());
        drop(held);
        assert!(slot.is_free() && slot.take().is_some());
    }
}
