//! Which organisation a project's hello names on each cloud (`organisationFor` and `ownOrganisation` in the TypeScript
//! agent's `codeHost/index.ts`, environments.md section 2, organisations.md section 3.3).
//!
//! The id comes from the shared backlog's record (`organisationId`, with `organisationSlug` beside it once kept), else
//! config.json's. It is the id of whichever site picked it, read as the agent's own cloud's. Another cloud is told its
//! own id for the organisation with the same slug, from each cloud's own `GET /api/agents/organisations` answer. None
//! is sent while either answer is unknown, so the hub never gets an id from another environment (it would read it as an
//! organisation that is gone); the hello goes again when an answer arrives or changes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::watch;

/// One organisation a cloud said the person is in.
#[derive(Debug, Clone, PartialEq)]
pub struct Organisation {
    pub id: u64,
    pub slug: String,
}

#[derive(Default)]
struct Inner {
    /// The agent's own cloud, as the store's ids go by.
    primary: Option<String>,
    /// Each cloud's last answer, by its url.
    answers: HashMap<String, Vec<Organisation>>,
    /// Each project's record: its `organisationId` and `organisationSlug`.
    store: HashMap<String, (u64, Option<String>)>,
}

#[derive(Clone, Default)]
pub struct StoreOrganisations(Arc<Mutex<Inner>>, Arc<watch::Sender<u64>>);

impl std::fmt::Debug for StoreOrganisations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreOrganisations")
    }
}

/// `organisationsBy`'s answer as the list: the `organisations` of `GET /api/agents/organisations`, with their slugs.
pub fn parse_answer(answer: &Value) -> Option<Vec<Organisation>> {
    answer
        .get("organisations")?
        .as_array()?
        .iter()
        .map(|o| {
            Some(Organisation {
                id: o.get("id")?.as_u64()?,
                slug: o.get("slug")?.as_str()?.to_string(),
            })
        })
        .collect()
}

impl StoreOrganisations {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wake(&self) {
        self.1.send_modify(|n| *n += 1);
    }

    /// The agent's own cloud, the one whose ids the store's record uses. The first is kept.
    pub fn set_primary(&self, url: &str) {
        let mut inner = self.lock();
        if inner.primary.is_none() {
            inner.primary = Some(url.to_string());
        }
    }

    pub fn primary(&self) -> Option<String> {
        self.lock().primary.clone()
    }

    /// A successful `settings_get` of `project`: the record's `organisationId` and `organisationSlug` (`None`: none saved,
    /// or not a number). A change wakes the connections, which say hello again if theirs differs.
    pub fn set(&self, project: &str, id: Option<u64>, slug: Option<String>) {
        let changed = {
            let mut inner = self.lock();
            match id {
                Some(id) => inner.store.insert(project.to_string(), (id, slug.clone())) != Some((id, slug)),
                None => inner.store.remove(project).is_some(),
            }
        };
        if changed {
            self.wake();
        }
    }

    /// What `url` said about the person's organisations. A change wakes the connections.
    pub fn learned(&self, url: &str, organisations: Vec<Organisation>) {
        let changed = self.lock().answers.insert(url.to_string(), organisations.clone()) != Some(organisations);
        if changed {
            self.wake();
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.1.subscribe()
    }

    /// The organisation `project`'s hello to the cloud at `url` names: the record's, else `config`'s (config.json's
    /// `organisationId`), as `url`'s own id for it. None while it isn't known there.
    pub fn for_cloud(&self, project: &str, url: &str, config: Option<u64>) -> Option<u64> {
        let inner = self.lock();
        let (id, slug) = match inner.store.get(project) {
            Some((id, slug)) => (*id, slug.as_deref()),
            None => (config?, None),
        };
        let own = inner.answers.get(inner.primary.as_deref()?)?;
        let others = inner
            .answers
            .iter()
            .filter(|(u, _)| Some(u.as_str()) != inner.primary.as_deref());
        let id = own_organisation(id, slug, own, others.flat_map(|(_, o)| o));
        if inner.primary.as_deref() == Some(url) {
            return Some(id);
        }
        let slug = &own.iter().find(|o| o.id == id)?.slug;
        inner.answers.get(url)?.iter().find(|o| &o.slug == slug).map(|o| o.id)
    }
}

/// The store's organisation as the agent's own cloud's id (`ownOrganisation`): the slug beside it names it there; a
/// record from before the slug was kept is read as another environment's id when the own cloud has no such id. The id as
/// stored when no organisation of the own cloud has that slug.
fn own_organisation<'a>(
    id: u64,
    slug: Option<&str>,
    own: &[Organisation],
    others: impl Iterator<Item = &'a Organisation>,
) -> u64 {
    let named = match slug {
        Some(s) => Some(s.to_string()),
        None if own.iter().any(|o| o.id == id) => None,
        None => others.into_iter().find(|o| o.id == id).map(|o| o.slug.clone()),
    };
    named
        .and_then(|n| own.iter().find(|o| o.slug == n).map(|o| o.id))
        .unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org(id: u64, slug: &str) -> Organisation {
        Organisation { id, slug: slug.into() }
    }

    /// Production (the own cloud) knows organisations 1 and 3; staging has the same person's Retaskable as 2.
    fn two_clouds() -> StoreOrganisations {
        let o = StoreOrganisations::default();
        o.set_primary("prod");
        o.learned("prod", vec![org(1, "jamez667"), org(3, "retaskable")]);
        o.learned("staging", vec![org(1, "jamez667"), org(2, "retaskable")]);
        o
    }

    #[test]
    fn a_new_value_wakes_subscribers_and_the_same_one_does_not() {
        let o = StoreOrganisations::default();
        let mut rx = o.subscribe();
        o.set("p", Some(3), None);
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        o.set("p", Some(3), None);
        o.learned("a", vec![org(3, "x")]);
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        o.learned("a", vec![org(3, "x")]);
        assert!(!rx.has_changed().unwrap());
        o.set("p", None, None);
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn nothing_is_named_while_the_own_clouds_answer_or_the_other_clouds_is_unknown() {
        let o = StoreOrganisations::default();
        o.set("p", Some(3), Some("retaskable".into()));
        assert_eq!(o.for_cloud("p", "prod", None), None, "no primary");
        o.set_primary("prod");
        assert_eq!(o.for_cloud("p", "prod", None), None, "own cloud hasn't answered");
        o.learned("prod", vec![org(3, "retaskable")]);
        assert_eq!(o.for_cloud("p", "prod", None), Some(3));
        assert_eq!(o.for_cloud("p", "staging", None), None, "staging hasn't answered");
        o.learned("staging", vec![org(1, "jamez667")]);
        assert_eq!(
            o.for_cloud("p", "staging", None),
            None,
            "staging has no such organisation"
        );
    }

    #[test]
    fn the_own_clouds_id_is_mapped_by_slug_for_every_other_cloud() {
        let o = two_clouds();
        o.set("p", Some(3), None);
        assert_eq!(o.for_cloud("p", "prod", None), Some(3));
        assert_eq!(o.for_cloud("p", "staging", None), Some(2));
    }

    #[test]
    fn a_store_id_from_another_environment_is_read_through_its_slug() {
        let o = two_clouds();
        // Picked on staging (id 2, the slug not kept): it is not an id of production's, but staging's names it.
        o.set("p", Some(2), None);
        assert_eq!(o.for_cloud("p", "prod", None), Some(3));
        assert_eq!(o.for_cloud("p", "staging", None), Some(2));
        // With the slug kept beside it, the slug decides.
        o.set("p", Some(2), Some("retaskable".into()));
        assert_eq!(o.for_cloud("p", "prod", None), Some(3));
    }

    #[test]
    fn the_record_wins_over_config_and_config_stands_alone() {
        let o = two_clouds();
        assert_eq!(o.for_cloud("p", "prod", Some(1)), Some(1));
        o.set("p", Some(3), None);
        assert_eq!(o.for_cloud("p", "prod", Some(1)), Some(3));
        assert_eq!(o.for_cloud("q", "staging", Some(1)), Some(1));
        assert_eq!(o.for_cloud("q", "prod", None), None);
    }

    #[test]
    fn an_answer_is_read_into_its_organisations() {
        let answer = serde_json::json!({ "organisations": [{ "id": 3, "slug": "r", "name": "R" }], "projects": {} });
        assert_eq!(parse_answer(&answer), Some(vec![org(3, "r")]));
        assert_eq!(
            parse_answer(&serde_json::json!({ "organisations": [{ "id": 3 }] })),
            None
        );
        assert_eq!(parse_answer(&serde_json::json!({})), None);
    }
}
