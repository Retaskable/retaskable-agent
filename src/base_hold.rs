//! The base checkout's busy lock ([`crate::lanes::base_checkout_path`]): Shoot and Play main each
//! hold it while they use the checkout, so each is refused while the other has it.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use crate::sync::lock;

static BASE_HELD: LazyLock<Mutex<HashMap<String, &'static str>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// A project's hold on its base checkout ([`hold_base`]); released on drop.
pub struct BaseHold(String);

impl Drop for BaseHold {
    fn drop(&mut self) {
        lock(&BASE_HELD).remove(&self.0);
    }
}

/// Takes `project`'s base checkout for `holder` ("a shoot", "Play main"), or says who has it.
pub fn hold_base(project: &str, holder: &'static str) -> Result<BaseHold, String> {
    let mut held = lock(&BASE_HELD);
    if let Some(who) = held.get(project) {
        return Err(format!("The base checkout is busy: {who} is using it."));
    }
    held.insert(project.to_string(), holder);
    Ok(BaseHold(project.to_string()))
}

/// Who holds `project`'s base checkout, if anyone.
pub fn base_holder(project: &str) -> Option<&'static str> {
    lock(&BASE_HELD).get(project).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_held_base_refuses_a_second_holder_and_names_the_first() {
        let first = hold_base("base-hold-t1", "a shoot").unwrap();
        assert_eq!(base_holder("base-hold-t1"), Some("a shoot"));
        let err = hold_base("base-hold-t1", "Play main").err().unwrap();
        assert_eq!(err, "The base checkout is busy: a shoot is using it.");
        drop(first);
    }

    #[test]
    fn dropping_the_hold_frees_the_base() {
        drop(hold_base("base-hold-t2", "Play main").unwrap());
        assert_eq!(base_holder("base-hold-t2"), None);
        assert!(hold_base("base-hold-t2", "a shoot").is_ok());
    }

    #[test]
    fn projects_hold_their_bases_independently() {
        let _a = hold_base("base-hold-t3a", "a shoot").unwrap();
        assert!(hold_base("base-hold-t3b", "Play main").is_ok());
    }
}
