//! Shared fixtures for this module's own tests: an [`Item`] builder and a couple of [`CheckConfig`]s.
use crate::backlog::{Item, ItemStatus};
use crate::config::CheckConfig;

pub(crate) fn item(spec_refs: &[&str], remaining: Option<&str>) -> Item {
    Item {
        id: "implementer-session".to_string(),
        title: "Have a session write the code".to_string(),
        status: ItemStatus::Building,
        rank: 1,
        do_next: false,
        depends_on: Vec::new(),
        spec_refs: spec_refs.iter().map(|s| s.to_string()).collect(),
        slice: 2,
        remaining: remaining.map(|s| s.to_string()),
        pr_url: None,
        branch: None,
        kind: None,
        milestone: None,
        reopened: None,
    }
}

pub(crate) fn checks() -> Vec<CheckConfig> {
    vec![
        CheckConfig {
            name: "test".to_string(),
            command: "cargo test".to_string(),
            blocking: true,
            timeout_sec: 0,
        },
        CheckConfig {
            name: "clippy".to_string(),
            command: "cargo clippy".to_string(),
            blocking: false,
            timeout_sec: 0,
        },
    ]
}
