//! The read kinds (docs/specs/intent-rows.md section 5.2; `intentCommand`'s default arm in the TS agent's
//! `intentPuller.ts`): `read_file`, `issues_preview` and `environments` rows, which any computer with the repository (or
//! a signed-in `gh`) claims, runs the same read as the pushed command ([`crate::read_kinds`]) and finishes with the
//! answer as the row's result. `environments` is claimed only with a `deploy` section (`intentCan`), and what it
//! answers is also kept as the `environments` fact with its age ([`crate::kept_facts`]), so the panel is never empty.
//! They start no work, so a pause or a Claude sign-out holds none back. A read row's result is at most 200 KB (the
//! store refuses more), so a larger answer is refused in words rather than lost on its way.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;

use super::draft_wanted::RowCx;
use super::{Handler, Outcome};
use crate::backlog::intents::IntentRow;
use crate::read_kinds::{self, ReadError, Reading};

/// What the store holds of a read row's result (`INTENT_READ_RESULT_MAX_BYTES`; other kinds hold 4 KB).
const RESULT_MAX: usize = 200 * 1024;

/// Whether the project has a `deploy` section: staging, production or where merges go (`cfg.deploy` in the TS agent).
pub fn has_deploy(deploy: &crate::config::DeployConfig) -> bool {
    deploy.merges_to.is_some() || deploy.staging.is_some() || deploy.production.is_some()
}

/// The handler of one read kind. With a flag (`environments`), a computer whose project has no `deploy` section leaves
/// the row open for one that has.
pub struct ReadHandler(pub &'static str, pub Option<Arc<AtomicBool>>);

impl Handler<RowCx> for ReadHandler {
    fn kind(&self) -> &'static str {
        self.0
    }

    fn takes(&self, _row: &IntentRow) -> bool {
        self.1.as_ref().is_none_or(|has| has.load(Ordering::SeqCst))
    }

    fn run(&self, row: IntentRow, cx: &RowCx) -> BoxFuture<'static, Outcome> {
        let kind = self.0;
        let facts = cx.facts.clone();
        let reading = Reading {
            client: cx.client.clone(),
            repo: cx.repo,
            token: cx.token.clone(),
            project: cx.project.clone(),
            gh: "gh".into(),
            deploy_log: cx.deploy_log.clone(),
        };
        async move {
            let read = read_kinds::read(kind, &row.args, &reading).await;
            keep(kind, &read, &facts);
            outcome(read)
        }
        .boxed()
    }
}

/// The Environments read's answer is kept as a fact beside the row's result (`keepEnvironments`); no other kind, and no
/// refusal or failure, is.
fn keep(kind: &str, read: &Result<serde_json::Value, ReadError>, facts: &crate::kept_facts::KeptFacts) {
    if let (Ok(view), "environments") = (read, kind) {
        facts.keep_environments(view);
    }
}

fn outcome(done: Result<serde_json::Value, ReadError>) -> Outcome {
    match done {
        Ok(v) if v.to_string().len() > RESULT_MAX => {
            Outcome::Refused("That answer is over 200 KB, more than a read holds.".into())
        }
        Ok(v) => Outcome::Done(v),
        Err(ReadError::Refused(why)) => Outcome::Refused(why),
        Err(ReadError::Failed(why)) => Outcome::Failed(why),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::config::{DeployConfig, DeployTarget};
    use crate::kept_facts::KeptFacts;

    fn row(kind: &str) -> IntentRow {
        IntentRow {
            id: "in-1".into(),
            kind: kind.into(),
            args: json!({}),
            issued_by: None,
            status: "claimed".into(),
        }
    }

    #[test]
    fn an_environments_row_is_left_open_without_a_deploy_section() {
        let has = Arc::new(AtomicBool::new(false));
        let handler = ReadHandler("environments", Some(has.clone()));
        assert!(!handler.takes(&row("environments")), "another computer's to take");
        has.store(true, Ordering::SeqCst);
        assert!(handler.takes(&row("environments")));
        assert!(
            ReadHandler("read_file", None).takes(&row("read_file")),
            "the others need no deploy section"
        );
    }

    #[test]
    fn a_deploy_section_is_any_of_its_three_parts() {
        let target = DeployTarget {
            branch: "staging".into(),
            url: None,
            smoke: None,
            version_path: None,
            rollout: None,
        };
        assert!(!has_deploy(&DeployConfig::default()));
        assert!(has_deploy(&DeployConfig {
            merges_to: Some("production".into()),
            ..Default::default()
        }));
        assert!(has_deploy(&DeployConfig {
            staging: Some(target),
            ..Default::default()
        }));
    }

    #[test]
    fn only_a_good_environments_answer_is_kept_as_a_fact() {
        let facts = KeptFacts::default();
        keep("read_file", &Ok(json!({ "content": "x" })), &facts);
        keep("environments", &Err(ReadError::Refused("no".into())), &facts);
        assert!(facts.owing().is_empty());
        keep("environments", &Ok(json!({ "staging": { "branch": "s" } })), &facts);
        assert_eq!(facts.owing(), vec!["environments"]);
    }

    #[test]
    fn a_read_ends_the_row_as_the_read_did() {
        assert_eq!(outcome(Ok(json!({ "a": 1 }))), Outcome::Done(json!({ "a": 1 })));
        assert_eq!(
            outcome(Err(ReadError::Refused("no".into()))),
            Outcome::Refused("no".into())
        );
        assert_eq!(
            outcome(Err(ReadError::Failed("broke".into()))),
            Outcome::Failed("broke".into())
        );
        let big = json!({ "content": "x".repeat(RESULT_MAX + 1) });
        assert!(matches!(outcome(Ok(big)), Outcome::Refused(_)));
        let fits = json!({ "content": "x".repeat(150 * 1024) });
        assert!(
            matches!(outcome(Ok(fits)), Outcome::Done(_)),
            "a read row holds up to 200 KB"
        );
    }
}
