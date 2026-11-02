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
