//! Where a day's raw Lighthouse reports live (last-local-state.md section 3.9; `lighthouseReports.ts` and the
//! flush in `lighthouseChore.ts`): written to `lighthouse/<day>/` on this computer by the run, uploaded to the
//! backlog store and deleted once the store's sha-256 matches; while the store can't be reached the files stay
//! and the upload is tried again. The restart read-back (`storedSource`) is not part of a job and is not here.
use std::collections::HashSet;
use std::future::Future;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

/// How often the reports are tried at most, unless a pass is forced (`FLUSH_EVERY_MS`).
pub const FLUSH_EVERY: Duration = Duration::from_secs(30);

/// A store that refused or could not take a report.
#[derive(Debug, Clone, PartialEq)]
pub struct PutError {
    /// The HTTP status the store answered, when it did.
    pub status: Option<u16>,
    pub message: String,
}

impl PutError {
    /// A refusal the store won't change its mind about soon: the report is too big, or this isn't allowed to
    /// write. Any 4xx but 408 and 429.
    pub fn is_refusal(&self) -> bool {
        self.status
            .is_some_and(|s| (400..500).contains(&s) && s != 408 && s != 429)
    }
}

/// The backlog store's reports (`ReportStore.put`): the sha-256 (hex) the store holds the bytes under.
pub trait ReportStore {
    fn put(&self, day: &str, file: &str, bytes: Vec<u8>) -> impl Future<Output = Result<String, PutError>> + Send;
}

fn is_day(name: &str) -> bool {
    chrono::NaiveDate::parse_from_str(name, "%Y-%m-%d").is_ok() && name.len() == 10
}

/// The `lighthouse/<day>` folders on disk, oldest first.
fn days_on_disk(folder: &Path) -> Vec<String> {
    let mut days: Vec<String> = std::fs::read_dir(folder.join("lighthouse"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| is_day(n))
        .collect();
    days.sort();
    days
}

/// Uploads every report on disk and deletes each once the store's sha-256 for it matches ours (`uploadReports`).
/// A store that can't be reached ends the pass (the files wait for the next); a report the store refuses is
/// added to `refused` and left, so it isn't offered again until the agent restarts (the 14-day sweep,
/// [`crate::lighthouse::run::prune_reports`], still removes it). Answers how many were refused.
pub async fn upload_reports<S: ReportStore>(folder: &Path, store: &S, refused: &mut HashSet<String>) -> usize {
    let mut refusals = 0;
    for day in days_on_disk(folder) {
        let dir = folder.join("lighthouse").join(&day);
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|f| f.ends_with(".json"))
            .collect();
        files.sort();
        for file in files {
            let id = format!("{day}/{file}");
            if refused.contains(&id) {
                continue;
            }
            let Ok(bytes) = std::fs::read(dir.join(&file)) else {
                continue;
            };
            let sha = format!("{:x}", Sha256::digest(&bytes));
            match store.put(&day, &file, bytes).await {
                Ok(held) if held == sha => {
                    let _ = std::fs::remove_file(dir.join(&file));
                }
                Ok(_) => {}
                Err(e) if e.is_refusal() => {
                    refused.insert(id);
                    refusals += 1;
                }
                Err(_) => return refusals,
            }
        }
        // Not empty: reports are waiting in it.
        let _ = std::fs::remove_dir(&dir);
    }
    refusals
}

#[derive(Default)]
struct FlushState {
    last: Option<Instant>,
    flushing: bool,
    refused: HashSet<String>,
}

/// One project's upload bookkeeping (`lastFlush`, `flushing`, `refusedReports`): at most every 30 seconds, one
/// pass at a time, and the reports the store refused since the agent started.
#[derive(Default)]
pub struct ReportFlush(Mutex<FlushState>);

impl ReportFlush {
    /// Uploads the reports on disk at most every [`FLUSH_EVERY`], or at once with `force` (`flushReports`); a
    /// store that can't be reached leaves them for the next try. `None` for the store leaves them on disk (tests).
    pub async fn flush<S: ReportStore>(&self, folder: &Path, store: Option<&S>, force: bool) {
        let Some(store) = store else {
            return;
        };
        let mut refused = {
            let mut st = self.0.lock().unwrap();
            if st.flushing || (!force && st.last.is_some_and(|t| t.elapsed() < FLUSH_EVERY)) {
                return;
            }
            st.flushing = true;
            st.last = Some(Instant::now());
            std::mem::take(&mut st.refused)
        };
        upload_reports(folder, store, &mut refused).await;
        let mut st = self.0.lock().unwrap();
        st.flushing = false;
        st.refused = refused;
    }
}

#[cfg(test)]
mod tests;
