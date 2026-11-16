//! The read kinds (docs/specs/intent-rows.md section 5.2; `intentCommand`'s default arm in the TS agent's
//! `intentPuller.ts`): `read_file`, `issues_preview` rows, which any computer with the repository (or a signed-in `gh`)
//! claims, runs the same read as the pushed command ([`crate::read_kinds`]) and finishes with the answer as the row's
//! result. They start no work, so a pause or a Claude sign-out holds none back. A row's result is at most 4 KB (the
//! store refuses more), so a larger answer is refused in words rather than lost on its way.
use futures_util::FutureExt;
use futures_util::future::BoxFuture;

use super::draft_wanted::RowCx;
use super::{Handler, Outcome};
use crate::backlog::intents::IntentRow;
use crate::read_kinds::{self, ReadError, Reading};

/// What the store holds of a row's result (`INTENT_RESULT_MAX_BYTES`).
const RESULT_MAX: usize = 4 * 1024;

pub struct ReadHandler(pub &'static str);

impl Handler<RowCx> for ReadHandler {
    fn kind(&self) -> &'static str {
        self.0
    }

    fn run(&self, row: IntentRow, cx: &RowCx) -> BoxFuture<'static, Outcome> {
        let kind = self.0;
        let reading = Reading {
            client: cx.client.clone(),
            repo: cx.repo,
            token: cx.token.clone(),
            project: cx.project.clone(),
            gh: "gh".into(),
            deploy_log: None,
        };
        async move { outcome(read_kinds::read(kind, &row.args, &reading).await) }.boxed()
    }
}

fn outcome(done: Result<serde_json::Value, ReadError>) -> Outcome {
    match done {
        Ok(v) if v.to_string().len() > RESULT_MAX => {
            Outcome::Refused("That answer is over 4 KB, more than a request holds.".into())
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
        let big = json!({ "content": "x".repeat(5000) });
        assert!(matches!(outcome(Ok(big)), Outcome::Refused(_)));
    }
}
