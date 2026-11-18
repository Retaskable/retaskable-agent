//! Asking a cloud which organisations the linking person is in (`GET /api/agents/organisations`, organisations.md
//! section 3.3; `askOrganisations` in connector.ts), so a project's hello names the organisation by that cloud's own id
//! (`StoreOrganisations::for_cloud`). Asked when a connection opens, when the hub says they changed
//! (`organisations_changed`), every ten minutes while one is open, and again soon after a failed ask. One ask at a time.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::Value;

use super::Cloud;
use crate::link::TokenAnswer;
use crate::store_organisation::parse_answer;

/// How often a connected agent asks again (connector.ts `ORGANISATIONS_SAFETY_MS`).
const SAFETY: Duration = Duration::from_secs(10 * 60);

/// Runs for the life of the cloud's connector.
pub(super) async fn keep_asking(cloud: Arc<Cloud>) {
    let mut wait = SAFETY;
    loop {
        // A nudge, or the slow check, or the retry after a failure.
        let _ = tokio::time::timeout(wait, cloud.organisations_due.notified()).await;
        // Each of the person's hubs says it, so a burst of the same news is one ask.
        tokio::time::sleep(cloud.timing.live_coalesce).await;
        if cloud.open.load(Ordering::SeqCst) == 0 {
            wait = SAFETY;
            continue;
        }
        wait = match ask(&cloud).await {
            Ok(()) => SAFETY,
            Err(why) => {
                tracing::warn!("{}: asking for the organisations failed: {why}", cloud.label);
                cloud.timing.idle
            }
        };
    }
}

async fn ask(cloud: &Cloud) -> Result<(), String> {
    let TokenAnswer::Token { url, access_token } = cloud.tokens.access_token(None).await else {
        return Err("no access token".into());
    };
    let res = cloud
        .http
        .get(format!("{}/api/agents/organisations", url.trim_end_matches('/')))
        .bearer_auth(access_token)
        .timeout(cloud.timing.connect)
        .send()
        .await
        .map_err(|e| crate::link::cause(&e))?;
    if !res.status().is_success() {
        return Err(res.status().as_u16().to_string());
    }
    let body: Value = res.json().await.map_err(|e| format!("not JSON: {e}"))?;
    let organisations = parse_answer(&body).ok_or("the answer was malformed")?;
    cloud
        .view
        .item_commands
        .organisations
        .learned(&cloud.url, organisations);
    Ok(())
}
