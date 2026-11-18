//! An issue tracker's key (issue-import.md section 3.5): one of the agent's secrets, saved sealed under the data key or
//! set in its environment (a saved one wins, `secrets/mod.rs`), so by the time the agent reads it the process
//! environment holds whichever applies. Never in `.retaskable/` in the clear, never in a command line and never sent
//! to retaskable.com. The TypeScript agent keeps them in the OS credential store under `retaskable-linear` and
//! `retaskable-jira`; here they are secrets of the names below, set with `retaskable secret set <NAME>` or on the
//! settings page (any plain name is allowed, `secrets/names.rs`).
use crate::env::Env;

/// A tracker that is read with this computer's own key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyed {
    Linear,
    Jira,
}

/// Linear's key is an API key.
pub const LINEAR_KEY: &str = "LINEAR_API_KEY";
/// Jira's is `<site> <email>:<API token>`: kept with the site it was checked on and sent nowhere else (a shared
/// setting could name any host).
pub const JIRA_KEY: &str = "JIRA_API_KEY";

impl Keyed {
    /// The secret's name.
    pub fn secret(self) -> &'static str {
        match self {
            Self::Linear => LINEAR_KEY,
            Self::Jira => JIRA_KEY,
        }
    }
}

/// Where a tracker's key comes from (a test hands a fake).
pub trait Keys: Send + Sync {
    fn get(&self, tracker: Keyed) -> Option<String>;
}

/// The agent's own: the secret as the process environment holds it, an empty one counting as none.
pub struct EnvKeys(pub Env);

impl EnvKeys {
    pub fn process() -> Self {
        Self(Env::from_process())
    }
}

impl Keys for EnvKeys {
    fn get(&self, tracker: Keyed) -> Option<String> {
        self.0
            .get(tracker.secret())
            .map(|k| k.trim().to_string())
            .filter(|k| !k.is_empty())
    }
}

#[cfg(test)]
pub(crate) struct FakeKeys(pub Option<String>, pub Option<String>);

#[cfg(test)]
impl Keys for FakeKeys {
    fn get(&self, tracker: Keyed) -> Option<String> {
        match tracker {
            Keyed::Linear => self.0.clone(),
            Keyed::Jira => self.1.clone(),
        }
    }
}
