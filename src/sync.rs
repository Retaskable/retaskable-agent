//! A tiny shared helper for locking a `Mutex` that this agent never wants to propagate a
//! poison from: every lock here only ever guards plain in-memory state (a status map, a
//! picture cache, a fetch timestamp), so a prior panic while holding one shouldn't poison it
//! for every later caller. Recovering the guard is what every call site in galleries.rs and
//! lanes.rs did by hand before this was pulled out.
use std::sync::{Mutex, MutexGuard};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovers_a_poisoned_mutex_instead_of_panicking() {
        let mutex = Mutex::new(0);
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = mutex.lock().unwrap();
            panic!("poisoning the mutex");
        }));
        assert!(mutex.is_poisoned());

        *lock(&mutex) += 1;

        assert_eq!(*lock(&mutex), 1);
    }
}
