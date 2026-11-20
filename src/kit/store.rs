//! A project's kit as this computer keeps it (docs/specs/project-kits.md section 2, TS `kitStore.ts`): read from the store's
//! `project_kit` fact, refreshed only when the fingerprint of the discovery inputs differs from the stored one (or on demand), and
//! written back. Read-only on the checkout; the optional session has no tools. Any failure is the wireframe kit, so a design review
//! is never blocked by it.
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

use super::discovery::{choose, discover, scan};
use super::{Genre, Kit, Tier, read_kit, wireframe_kit};
use crate::backlog::Client;

/// How long a computer trusts its last look at the checkout before it compares fingerprints again.
pub const KIT_LOOK: Duration = Duration::from_secs(10 * 60);

static MEMO: Mutex<Option<HashMap<PathBuf, (Instant, Kit)>>> = Mutex::new(None);

/// The store the kit's fact lives in, as this computer reaches it.
pub struct KitStore<'a> {
    pub client: &'a Client,
    pub repo: u64,
    pub token: &'a str,
    pub machine: &'a str,
}

async fn stored(store: &KitStore<'_>) -> Option<Kit> {
    let rows = store
        .client
        .facts_get(store.repo, Some(store.token), &["project_kit"])
        .await
        .ok()?;
    read_kit(&rows.get("project_kit")?.data)
}

fn remembered(root: &Path, now: Instant) -> Option<Kit> {
    let memo = MEMO.lock().unwrap_or_else(|e| e.into_inner());
    let (at, kit) = memo.as_ref()?.get(root)?;
    (now.duration_since(*at) < KIT_LOOK).then(|| kit.clone())
}

fn remember(root: &Path, now: Instant, kit: &Kit) {
    MEMO.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(Default::default)
        .insert(root.to_path_buf(), (now, kit.clone()));
}

/// Forgets what this computer last saw (tests).
pub fn forget() {
    MEMO.lock().unwrap_or_else(|e| e.into_inner()).take();
}

/// The kit for the checkout at `root`: the stored one while its fingerprint still matches, else a fresh discovery (stored when it is
/// a catalogue or wireframe). `force` is the on-demand refresh. `summarise` is the session that tidies a catalogue (its prompt in,
/// its structured answer out). Never fails: the wireframe kit stands in.
pub async fn kit_for<F, Fut>(root: &Path, store: Option<&KitStore<'_>>, summarise: Option<F>, force: bool) -> Kit
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Option<Value>>,
{
    let now = Instant::now();
    if !force && let Some(kit) = remembered(root, now) {
        return kit;
    }
    let owned = root.to_path_buf();
    let Ok(found) = tokio::task::spawn_blocking(move || scan(&owned)).await else {
        return wireframe_kit(Genre::App, None, None, None);
    };
    let current = choose(&found);
    if !force
        && let Some(store) = store
        && let Some(have) = stored(store).await
        && have.fingerprint.as_deref() == Some(found.fingerprint.as_str())
        && have.tier == current.tier
    {
        remember(root, now, &have);
        return have;
    }
    let ask = (current.tier == Tier::Catalogue).then_some(summarise).flatten();
    let (kit, _how) = discover(&found, ask).await;
    remember(root, now, &kit);
    if kit.tier != Tier::Library
        && let Some(store) = store
        && let Ok(data) = serde_json::to_value(&kit)
    {
        let _ = store
            .client
            .fact_set(store.repo, Some(store.token), "project_kit", &data, store.machine, None)
            .await;
    }
    kit
}
