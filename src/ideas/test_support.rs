//! Shared fixtures for this module's own tests: a waiting [`Idea`].
use super::model::Idea;

pub(crate) fn waiting_idea(title: &str, waited: u32) -> Idea {
    Idea {
        id: title.to_string(),
        title: title.to_string(),
        experience: String::new(),
        why: String::new(),
        why_now: String::new(),
        for_profiles: vec![],
        feature: None,
        new_feature: None,
        waited,
    }
}
