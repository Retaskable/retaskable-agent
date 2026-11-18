//! A day's raw Lighthouse report in the store (last-local-state.md section 3.9, `uploadReport` in the TypeScript
//! `backlogStore.ts`): `POST /v1/repos/<id>/lighthouse/<day>/<file>` with the file's bytes, answering the sha-256
//! the store holds them under, which the caller checks before it deletes the file.
use serde_json::Value;

use super::{BacklogError, Client};

/// A refusal's code the report's caller treats as final (any 4xx but a timeout or a rate limit is the store's).
fn refused(status: u16, body: &Value) -> BacklogError {
    let text = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string);
    BacklogError::BadAnswer {
        status,
        code: text("error"),
        message: text("message")
            .or_else(|| text("error"))
            .unwrap_or_else(|| format!("the store answered {status}")),
    }
}

impl Client {
    /// Keeps one day's report file in the store: the sha-256 (hex) it answers, or why not. A path-safe `file` is not
    /// assumed: each segment is percent-encoded.
    pub async fn upload_report(
        &self,
        repo_id: u64,
        token: Option<&str>,
        day: &str,
        file: &str,
        bytes: Vec<u8>,
    ) -> Result<String, BacklogError> {
        let mut url = reqwest::Url::parse(&format!("{}/v1/repos/{repo_id}", self.base_url))
            .map_err(|e| BacklogError::Unreachable(format!("the store's address isn't a url: {e}")))?;
        url.path_segments_mut()
            .map_err(|()| BacklogError::Unreachable("the store's address can't take a path".into()))?
            .extend(["lighthouse", day, file]);
        let res = self
            .send_bearer(token, |t| {
                let req = self
                    .http
                    .client()
                    .post(url.clone())
                    .header("Content-Type", "application/json")
                    .body(bytes.clone());
                match t {
                    Some(t) => req.bearer_auth(t),
                    None => req,
                }
            })
            .await
            .map_err(|e| BacklogError::Unreachable(crate::http_client::chain(&e)))?;
        let status = res.status();
        let body: Value = res.json().await.unwrap_or(Value::Null);
        match body.pointer("/value/hash").and_then(Value::as_str) {
            Some(hash) if status.is_success() => Ok(hash.to_string()),
            _ => Err(refused(status.as_u16(), &body)),
        }
    }
}

#[cfg(test)]
mod tests;
