//! The organisation each project's shared backlog names (`organisationId` in the store's settings record), as the loop
//! last read it. The hello reports it as `project.organisationId`, so the hub attaches the project in that organisation
//! and not in the user's oldest (cloud.md section 5.1, organisations.md section 3.3). The store's value wins over
//! config.json's, as in the TypeScript agent's `applyStoreSettings`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

#[derive(Clone, Default)]
pub struct StoreOrganisations(Arc<Mutex<HashMap<String, u64>>>, Arc<watch::Sender<u64>>);

impl std::fmt::Debug for StoreOrganisations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreOrganisations")
    }
}

impl StoreOrganisations {
    /// A successful `settings_get` of `project`: the record's `organisationId` (`None`: none saved, or not a number).
    /// A change wakes the connections, which say hello again.
    pub fn set(&self, project: &str, id: Option<u64>) {
        let changed = {
            let mut map = self.0.lock().unwrap_or_else(|e| e.into_inner());
            match id {
                Some(id) => map.insert(project.to_string(), id) != Some(id),
                None => map.remove(project).is_some(),
            }
        };
        if changed {
            self.1.send_modify(|n| *n += 1);
        }
    }

    pub fn get(&self, project: &str) -> Option<u64> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).get(project).copied()
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.1.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_value_wakes_subscribers_and_the_same_one_does_not() {
        let o = StoreOrganisations::default();
        let mut rx = o.subscribe();
        assert_eq!(o.get("p"), None);
        o.set("p", Some(3));
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        o.set("p", Some(3));
        assert!(!rx.has_changed().unwrap());
        assert_eq!(o.get("p"), Some(3));
        o.set("p", None);
        assert!(rx.has_changed().unwrap());
        assert_eq!(o.get("p"), None);
    }
}
