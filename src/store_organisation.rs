//! Which organisation a project's hello names (`organisationSlugOf`, `ownOrganisation` and `helloProject` in the TypeScript
//! agent's `codeHost/index.ts`; docs/specs/organisation-by-slug.md). An organisation's id belongs to one cloud, so every cloud
//! is told the **slug**, which each hub maps to its own id. The agent's own cloud (the first it connects to) is also
//! told its own id for it, for a hub that still reads only the id; no other cloud ever gets an id, and no other cloud's
//! `GET /api/agents/organisations` answer is kept or used.
//!
//! The slug is the shared backlog's record's `organisationSlug`, else the slug the own cloud's answer gives for the id in
//! config.json (`organisationId`), else the `organisationSlug` config.json kept. None is sent while none is known; the
//! hello goes again when the record or the own cloud's answer arrives or changes. An `organisationId` in the record is
//! another cloud's number and is ignored.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::watch;

/// The most the hub takes of a slug in a hello (`organisationSlug: z.string().max(100)`).
const SLUG_MAX: usize = 100;

/// One organisation a cloud said the person is in.
#[derive(Debug, Clone, PartialEq)]
pub struct Organisation {
    pub id: u64,
    pub slug: String,
}

/// What config.json names for a project: its own cloud's id for the organisation, and the slug it kept.
#[derive(Debug, Clone, Copy, Default)]
pub struct Configured<'a> {
    pub id: Option<u64>,
    pub slug: Option<&'a str>,
}

/// The organisation a link names, as the project's config keeps it: the own cloud's id for it and its slug.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pick {
    pub id: Option<u64>,
    pub slug: Option<String>,
}

/// A link named an organisation the person isn't in on the agent's own cloud.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotOurs;

/// What one project's hello to one cloud says of its organisation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Naming {
    /// The slug, the same on every cloud: the hub maps it to its own id, and it wins over the id.
    pub slug: Option<String>,
    /// The agent's own cloud's id for it, sent to that cloud only.
    pub own_id: Option<u64>,
}

#[derive(Default)]
struct Inner {
    /// The agent's own cloud, as config.json's ids go by.
    primary: Option<String>,
    /// The own cloud's last answer. No other cloud's is kept.
    own: Option<Vec<Organisation>>,
    /// Each project's record's `organisationSlug`.
    store: HashMap<String, String>,
}

#[derive(Clone, Default)]
pub struct StoreOrganisations(Arc<Mutex<Inner>>, Arc<watch::Sender<u64>>);

impl std::fmt::Debug for StoreOrganisations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreOrganisations")
    }
}

/// The answer's list: the `organisations` of `GET /api/agents/organisations`, with their slugs.
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
        crate::sync::lock(&self.0)
    }

    fn wake(&self) {
        self.1.send_modify(|n| *n += 1);
    }

    /// The agent's own cloud, the one whose ids config.json uses. The first is kept.
    pub fn set_primary(&self, url: &str) {
        let mut inner = self.lock();
        if inner.primary.is_none() {
            inner.primary = Some(url.to_string());
        }
    }

    pub fn primary(&self) -> Option<String> {
        self.lock().primary.clone()
    }

    /// Whether `url` is the agent's own cloud.
    pub fn is_primary(&self, url: &str) -> bool {
        self.lock().primary.as_deref() == Some(url)
    }

    /// A successful `settings_get` of `project`: the record's `organisationSlug` (`None`: none saved). A change wakes the
    /// connections, which say hello again if theirs differs.
    pub fn set(&self, project: &str, slug: Option<String>) {
        let changed = {
            let mut inner = self.lock();
            match slug.filter(|s| !s.is_empty()) {
                Some(slug) => inner.store.insert(project.to_string(), slug.clone()) != Some(slug),
                None => inner.store.remove(project).is_some(),
            }
        };
        if changed {
            self.wake();
        }
    }

    /// What `url` said about the person's organisations. Only the own cloud's is kept: another cloud's ids are its own and
    /// no hello needs them. A change wakes the connections.
    pub fn learned(&self, url: &str, organisations: Vec<Organisation>) {
        let changed = {
            let mut inner = self.lock();
            if inner.primary.as_deref() != Some(url) {
                return;
            }
            inner.own.replace(organisations.clone()) != Some(organisations)
        };
        if changed {
            self.wake();
        }
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.1.subscribe()
    }

    /// The own cloud's id for the organisation with `slug`, once it has answered and has one.
    pub fn own_id_of(&self, slug: &str) -> Option<u64> {
        self.lock().own.as_ref()?.iter().find(|o| o.slug == slug).map(|o| o.id)
    }

    /// The slug the own cloud's answer gives for its id `id`.
    pub fn own_slug_of(&self, id: u64) -> Option<String> {
        let inner = self.lock();
        inner.own.as_ref()?.iter().find(|o| o.id == id).map(|o| o.slug.clone())
    }

    /// Whether the own cloud has answered.
    pub fn own_known(&self) -> bool {
        self.lock().own.is_some()
    }

    /// The organisation a `link_project` names, as the project's config keeps it (`organisation-by-slug.md` section 3.3):
    /// a `slug` wins over an `id`. With the own cloud's answer in, each is checked against it (`Err`: not one this person is
    /// in) and gives the other (an id for a slug, a slug for an id); without it the slug is kept alone, or the id as sent.
    pub fn pick(&self, slug: Option<&str>, id: Option<u64>) -> Result<Pick, NotOurs> {
        let inner = self.lock();
        let own = inner.own.as_deref();
        match (slug, id, own) {
            (Some(slug), _, Some(own)) => own
                .iter()
                .find(|o| o.slug == slug)
                .map(|o| Pick {
                    id: Some(o.id),
                    slug: Some(o.slug.clone()),
                })
                .ok_or(NotOurs),
            (Some(slug), _, None) => Ok(Pick {
                id: None,
                slug: Some(slug.to_string()),
            }),
            (None, Some(id), Some(own)) => own
                .iter()
                .find(|o| o.id == id)
                .map(|o| Pick {
                    id: Some(o.id),
                    slug: Some(o.slug.clone()),
                })
                .ok_or(NotOurs),
            (None, Some(id), None) => Ok(Pick {
                id: Some(id),
                slug: None,
            }),
            (None, None, _) => Ok(Pick::default()),
        }
    }

    /// What `project`'s hello to the cloud at `url` says of its organisation. See the module's note for the slug. The own
    /// id goes to the own cloud only: the own cloud's id for the record's slug (the config's, as kept, while that cloud
    /// hasn't answered; none when it has no such organisation), else the config's.
    pub fn naming(&self, project: &str, url: &str, config: Configured<'_>) -> Naming {
        let inner = self.lock();
        let record = inner.store.get(project);
        let own = inner.own.as_deref();
        let slug = record
            .cloned()
            .or_else(|| {
                let id = config.id?;
                own?.iter().find(|o| o.id == id).map(|o| o.slug.clone())
            })
            .or_else(|| config.slug.map(str::to_string))
            .filter(|s| !s.is_empty() && s.chars().count() <= SLUG_MAX);
        let own_id = if inner.primary.as_deref() == Some(url) {
            match (record, own) {
                (Some(slug), Some(own)) => own.iter().find(|o| &o.slug == slug).map(|o| o.id),
                _ => config.id,
            }
        } else {
            None
        };
        Naming { slug, own_id }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org(id: u64, slug: &str) -> Organisation {
        Organisation { id, slug: slug.into() }
    }

    fn cfg(id: Option<u64>, slug: Option<&str>) -> Configured<'_> {
        Configured { id, slug }
    }

    /// Production (the own cloud) knows organisations 1 and 3; staging has the same person's Retaskable as 2, which
    /// this agent never asks about.
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
        o.set_primary("a");
        let mut rx = o.subscribe();
        o.set("p", Some("acme".into()));
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        o.set("p", Some("acme".into()));
        o.learned("a", vec![org(3, "x")]);
        assert!(rx.has_changed().unwrap());
        rx.mark_unchanged();
        o.learned("a", vec![org(3, "x")]);
        assert!(!rx.has_changed().unwrap());
        o.set("p", None);
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn another_clouds_answer_is_neither_kept_nor_woken_for() {
        let o = StoreOrganisations::default();
        o.set_primary("prod");
        let rx = o.subscribe();
        o.learned("staging", vec![org(2, "retaskable")]);
        assert!(!rx.has_changed().unwrap());
        assert!(!o.own_known());
    }

    #[test]
    fn every_cloud_is_told_the_records_slug_and_only_the_own_cloud_its_own_id() {
        let o = two_clouds();
        o.set("p", Some("retaskable".into()));
        let prod = o.naming("p", "prod", cfg(None, None));
        assert_eq!(
            prod,
            Naming {
                slug: Some("retaskable".into()),
                own_id: Some(3)
            }
        );
        // Staging's own id for it is the hub's to find: the agent names the slug alone.
        assert_eq!(
            o.naming("p", "staging", cfg(None, None)),
            Naming {
                slug: Some("retaskable".into()),
                own_id: None
            }
        );
        assert_eq!(
            o.naming("p", "local", cfg(Some(1), None)).own_id,
            None,
            "no other cloud is ever sent an id"
        );
    }

    #[test]
    fn the_slug_is_named_before_any_cloud_has_answered() {
        let o = StoreOrganisations::default();
        o.set_primary("prod");
        o.set("p", Some("retaskable".into()));
        // The own cloud hasn't answered: the slug goes now, and the id is the config's, as it was kept.
        assert_eq!(
            o.naming("p", "prod", cfg(Some(7), None)),
            Naming {
                slug: Some("retaskable".into()),
                own_id: Some(7)
            }
        );
        assert_eq!(
            o.naming("p", "staging", cfg(Some(7), None)).slug.as_deref(),
            Some("retaskable")
        );
    }

    #[test]
    fn a_record_slug_the_own_cloud_has_no_organisation_for_names_no_id_there() {
        let o = two_clouds();
        o.set("p", Some("nobody".into()));
        let prod = o.naming("p", "prod", cfg(Some(1), None));
        assert_eq!((prod.slug.as_deref(), prod.own_id), (Some("nobody"), None));
    }

    #[test]
    fn without_a_record_the_slug_is_the_own_clouds_for_the_configs_id_then_the_slug_config_kept() {
        let o = two_clouds();
        let named = o.naming("p", "staging", cfg(Some(3), Some("kept")));
        assert_eq!(
            named.slug.as_deref(),
            Some("retaskable"),
            "the own cloud's answer first"
        );
        assert_eq!(
            o.naming("p", "prod", cfg(Some(3), None)),
            Naming {
                slug: Some("retaskable".into()),
                own_id: Some(3)
            }
        );
        // An id the own cloud doesn't know falls to the slug kept; with none, nothing is said.
        assert_eq!(
            o.naming("p", "staging", cfg(Some(9), Some("kept"))).slug.as_deref(),
            Some("kept")
        );
        assert_eq!(o.naming("p", "staging", cfg(Some(9), None)).slug, None);
        // Before the own cloud answers, only the kept slug can be named.
        let early = StoreOrganisations::default();
        early.set_primary("prod");
        assert_eq!(early.naming("p", "prod", cfg(Some(3), None)).slug, None);
        assert_eq!(
            early.naming("p", "prod", cfg(Some(3), Some("kept"))).slug.as_deref(),
            Some("kept")
        );
    }

    #[test]
    fn the_record_wins_over_config_and_a_record_that_names_none_leaves_configs() {
        let o = two_clouds();
        assert_eq!(
            o.naming("p", "prod", cfg(Some(1), None)).slug.as_deref(),
            Some("jamez667")
        );
        o.set("p", Some("retaskable".into()));
        let named = o.naming("p", "prod", cfg(Some(1), None));
        assert_eq!((named.slug.as_deref(), named.own_id), (Some("retaskable"), Some(3)));
        o.set("p", None);
        assert_eq!(o.naming("p", "prod", cfg(Some(1), None)).own_id, Some(1));
        assert_eq!(o.naming("q", "prod", cfg(None, None)), Naming::default());
    }

    #[test]
    fn a_slug_the_hub_would_refuse_is_not_sent() {
        let o = two_clouds();
        o.set("p", Some("x".repeat(101)));
        assert_eq!(o.naming("p", "staging", cfg(None, None)).slug, None);
        o.set("p", Some("x".repeat(100)));
        assert_eq!(
            o.naming("p", "staging", cfg(None, None)).slug.map(|s| s.len()),
            Some(100)
        );
    }

    #[test]
    fn the_own_clouds_answer_is_read_both_ways() {
        let o = two_clouds();
        assert_eq!((o.own_id_of("retaskable"), o.own_id_of("nobody")), (Some(3), None));
        assert_eq!(o.own_slug_of(3).as_deref(), Some("retaskable"));
        assert_eq!(o.own_slug_of(2), None);
    }

    #[test]
    fn a_link_by_slug_is_checked_against_the_own_clouds_answer_and_a_slug_wins_over_an_id() {
        let o = two_clouds();
        let pick = |slug: Option<&str>, id| o.pick(slug, id);
        let retaskable = Pick {
            id: Some(3),
            slug: Some("retaskable".into()),
        };
        assert_eq!(pick(Some("retaskable"), None), Ok(retaskable.clone()));
        assert_eq!(
            pick(Some("retaskable"), Some(9)),
            Ok(retaskable.clone()),
            "the slug wins"
        );
        assert_eq!(pick(Some("nobody"), None), Err(NotOurs));
        assert_eq!(pick(None, Some(3)), Ok(retaskable), "an id gives its slug");
        assert_eq!(pick(None, Some(9)), Err(NotOurs));
        assert_eq!(pick(None, None), Ok(Pick::default()));
    }

    #[test]
    fn a_link_before_the_own_cloud_has_answered_keeps_what_it_was_sent() {
        let o = StoreOrganisations::default();
        o.set_primary("prod");
        assert_eq!(
            o.pick(Some("retaskable"), Some(9)),
            Ok(Pick {
                id: None,
                slug: Some("retaskable".into())
            })
        );
        assert_eq!(
            o.pick(None, Some(3)),
            Ok(Pick {
                id: Some(3),
                slug: None
            })
        );
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
