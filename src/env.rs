//! How the agent reads environment variables: one cheap-to-clone handle, so a test can hand any
//! function a fake environment and nothing but `main` ever touches the process's own.

use std::collections::HashMap;
use std::sync::Arc;

type Lookup = dyn Fn(&str) -> Option<String> + Send + Sync;

/// A lookup of environment variables by name. Cloning shares the same lookup.
#[derive(Clone)]
pub struct Env(Arc<Lookup>);

impl Env {
    /// An environment answered by `lookup`.
    pub fn new(lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        Self(Arc::new(lookup))
    }

    /// The process's own environment, with an empty variable counted as unset.
    pub fn from_process() -> Self {
        Self::new(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }

    /// The process's own environment exactly as set, an empty variable included.
    pub fn from_process_raw() -> Self {
        Self::new(|k| std::env::var(k).ok())
    }

    /// An environment holding exactly `vars`, for tests.
    pub fn from_map(vars: HashMap<String, String>) -> Self {
        Self::new(move |k| vars.get(k).cloned())
    }

    /// An environment holding exactly these `(name, value)` pairs, for tests.
    pub fn from_pairs<K: ToString, V: ToString>(pairs: impl IntoIterator<Item = (K, V)>) -> Self {
        Self::from_map(pairs.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    /// An environment with nothing set, for tests.
    pub fn empty() -> Self {
        Self::new(|_| None)
    }

    /// The variable `name`, or `None` when it's unset.
    pub fn get(&self, name: &str) -> Option<String> {
        (self.0)(name)
    }
}

impl std::fmt::Debug for Env {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Env(..)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_pairs_answers_only_what_it_holds() {
        let env = Env::from_pairs([("A", "1")]);
        assert_eq!(env.get("A").as_deref(), Some("1"));
        assert_eq!(env.get("B"), None);
    }

    #[test]
    fn a_clone_shares_the_lookup() {
        let env = Env::from_pairs([("A", "1")]).clone();
        assert_eq!(env.get("A").as_deref(), Some("1"));
    }

    #[test]
    fn empty_has_nothing() {
        assert_eq!(Env::empty().get("PATH"), None);
    }
}
