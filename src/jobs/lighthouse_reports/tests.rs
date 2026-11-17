use std::collections::HashMap;
use std::sync::Mutex;

use super::*;

/// A store that answers per file name: the sha it holds, or an error.
#[derive(Default)]
struct Fake {
    answers: Mutex<HashMap<String, Result<Option<String>, PutError>>>,
    puts: Mutex<Vec<String>>,
}

impl Fake {
    fn answer(&self, file: &str, a: Result<Option<String>, PutError>) {
        self.answers.lock().unwrap().insert(file.to_string(), a);
    }
}

impl ReportStore for Fake {
    async fn put(&self, day: &str, file: &str, bytes: Vec<u8>) -> Result<String, PutError> {
        self.puts.lock().unwrap().push(format!("{day}/{file}"));
        let a = self.answers.lock().unwrap().get(file).cloned();
        match a {
            Some(Ok(Some(sha))) => Ok(sha),
            Some(Ok(None)) | None => Ok(format!("{:x}", Sha256::digest(&bytes))),
            Some(Err(e)) => Err(e),
        }
    }
}

fn report(dir: &Path, day: &str, file: &str) {
    let d = dir.join("lighthouse").join(day);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(file), format!("{{\"f\":\"{file}\"}}")).unwrap();
}

fn err(status: Option<u16>) -> PutError {
    PutError {
        status,
        message: "no".into(),
    }
}

#[tokio::test]
async fn a_report_the_store_holds_under_our_sha_is_deleted_with_its_empty_day() {
    let tmp = tempfile::tempdir().unwrap();
    report(tmp.path(), "2026-11-08", "root-mobile.json");
    report(tmp.path(), "2026-11-08", "root-desktop.json");
    report(tmp.path(), "notes", "x.json");
    let store = Fake::default();
    let refused = upload_reports(tmp.path(), &store, &mut HashSet::new()).await;
    assert_eq!(refused, 0);
    assert!(!tmp.path().join("lighthouse/2026-11-08").exists());
    assert!(tmp.path().join("lighthouse/notes/x.json").exists(), "not a day folder");
    assert_eq!(store.puts.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn a_different_sha_keeps_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    report(tmp.path(), "2026-11-08", "a-mobile.json");
    let store = Fake::default();
    store.answer("a-mobile.json", Ok(Some("0".into())));
    upload_reports(tmp.path(), &store, &mut HashSet::new()).await;
    assert!(tmp.path().join("lighthouse/2026-11-08/a-mobile.json").exists());
}

#[tokio::test]
async fn a_refusal_is_remembered_and_the_pass_goes_on_while_an_outage_ends_it() {
    let tmp = tempfile::tempdir().unwrap();
    report(tmp.path(), "2026-11-07", "big-mobile.json");
    report(tmp.path(), "2026-11-08", "a-mobile.json");
    report(tmp.path(), "2026-11-08", "b-mobile.json");
    let store = Fake::default();
    store.answer("big-mobile.json", Err(err(Some(413))));
    store.answer("b-mobile.json", Err(err(None)));
    let mut refused = HashSet::new();
    let n = upload_reports(tmp.path(), &store, &mut refused).await;
    assert_eq!(n, 1);
    assert!(refused.contains("2026-11-07/big-mobile.json"));
    assert!(tmp.path().join("lighthouse/2026-11-07/big-mobile.json").exists());
    // a- went up; b- hit an outage, which ended the pass with b- left.
    assert!(!tmp.path().join("lighthouse/2026-11-08/a-mobile.json").exists());
    assert!(tmp.path().join("lighthouse/2026-11-08/b-mobile.json").exists());
    // A refused report isn't offered again.
    store.puts.lock().unwrap().clear();
    upload_reports(tmp.path(), &store, &mut refused).await;
    assert_eq!(*store.puts.lock().unwrap(), ["2026-11-08/b-mobile.json"]);
}

#[test]
fn which_statuses_are_refusals() {
    for (status, refusal) in [
        (Some(400), true),
        (Some(413), true),
        (Some(408), false),
        (Some(429), false),
        (Some(500), false),
        (None, false),
    ] {
        assert_eq!(err(status).is_refusal(), refusal, "{status:?}");
    }
}

#[tokio::test]
async fn flushing_is_throttled_unless_forced_and_skipped_with_no_store() {
    let tmp = tempfile::tempdir().unwrap();
    report(tmp.path(), "2026-11-08", "a-mobile.json");
    let flush = ReportFlush::default();
    flush.flush::<Fake>(tmp.path(), None, true).await;
    assert!(tmp.path().join("lighthouse/2026-11-08/a-mobile.json").exists());
    let store = Fake::default();
    store.answer("a-mobile.json", Err(err(None)));
    flush.flush(tmp.path(), Some(&store), false).await;
    assert_eq!(store.puts.lock().unwrap().len(), 1);
    // Tried a moment ago: left for the next time, however many calls come.
    flush.flush(tmp.path(), Some(&store), false).await;
    assert_eq!(store.puts.lock().unwrap().len(), 1);
    store.answer("a-mobile.json", Ok(None));
    flush.flush(tmp.path(), Some(&store), true).await;
    assert_eq!(store.puts.lock().unwrap().len(), 2);
    assert!(!tmp.path().join("lighthouse/2026-11-08").exists());
}
