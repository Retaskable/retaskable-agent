//! The local model's health check (docs/specs/agent/07-orchestrator-loop.md section 7.2's
//! "Is it up?"): while the computer's local model is turned on, `GET <baseUrl>/v1/models` every
//! minute, plain HTTP with a 5-second timeout. Picking a tier and actually routing a session onto
//! it while this check is failing (the "tiers act as if the fallback were off" part) is the
//! orchestrator's job, which doesn't exist yet; for now this only logs on each up/down transition,
//! the same pattern as `phone::watch`.
use std::time::Duration;

/// Checked every minute (section 7.2).
pub const POLL_INTERVAL: Duration = Duration::from_secs(60);
/// Plain HTTP, a 5-second timeout (section 7.2).
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

/// One check of `GET <base_url>/v1/models`: up only on a successful status, never guessed from a
/// partial answer. A timeout, connection refusal or any other transport error counts as down.
pub async fn check_up(http: &reqwest::Client, base_url: &str) -> bool {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    matches!(http.get(url).send().await, Ok(res) if res.status().is_success())
}

/// Watches the local model every `POLL_INTERVAL`, logging only when it goes up or down. Runs until
/// the process exits; called once per computer, not per project (section 7.2: "One local session
/// at a time" is machine-wide).
pub async fn watch(base_url: String) -> ! {
    let http = reqwest::Client::builder()
        .timeout(CHECK_TIMEOUT)
        .build()
        .expect("building the local model HTTP client");
    let mut last: Option<bool> = None;
    loop {
        let up = check_up(&http, &base_url).await;
        if last != Some(up) {
            if up {
                tracing::info!("Local model: up at {base_url}");
            } else {
                tracing::warn!("Local model: down");
            }
            last = Some(up);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};

    fn client() -> reqwest::Client {
        reqwest::Client::builder().timeout(CHECK_TIMEOUT).build().unwrap()
    }

    /// Starts a fake local model server answering `handler` on `GET /v1/models`, and gives its
    /// base URL.
    async fn fake_server(handler: axum::routing::MethodRouter) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new().route("/v1/models", handler);
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn check_up_is_true_only_on_a_successful_status() {
        let base = fake_server(get(|| async { "ok" })).await;
        assert!(check_up(&client(), &base).await);
    }

    #[tokio::test]
    async fn check_up_is_false_on_an_error_status() {
        let base = fake_server(get(|| async { (axum::http::StatusCode::INTERNAL_SERVER_ERROR, "") })).await;
        assert!(!check_up(&client(), &base).await);
    }

    #[tokio::test]
    async fn check_up_is_false_when_nothing_is_listening() {
        // A bound-then-dropped listener's port refuses the connection outright.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        assert!(!check_up(&client(), &format!("http://{addr}")).await);
    }

    #[tokio::test]
    async fn check_up_trims_a_trailing_slash_off_the_base_url() {
        let base = fake_server(get(|| async { "ok" })).await;
        assert!(check_up(&client(), &format!("{base}/")).await);
    }
}
