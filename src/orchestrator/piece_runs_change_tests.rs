//! `run_change`: a build or fix run tells the store its change as soon as it opens (pieces.md section 3.1).
use super::*;

/// `request` with the run's context pointed at a fake store, which records what the run sends.
async fn request_with_store(
    rig: &Rig,
    kind: RunKind,
    piece: Value,
    reply: Value,
    status: u16,
) -> (RunRequest, Started, crate::backlog::tests::Seen) {
    let (base, seen) = crate::backlog::tests::store(status, reply).await;
    let (mut req, started) = request(rig, kind, piece);
    req.context.client = Arc::new(crate::backlog::Client::new(base));
    (req, started, seen)
}

fn changes(seen: &crate::backlog::tests::Seen) -> Vec<Value> {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|b| b["op"] == "run_change")
        .map(|b| b["args"].clone())
        .collect()
}

#[tokio::test]
async fn a_build_run_tells_the_store_its_change_once_when_it_opens() {
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    let (req, mut s, seen) =
        request_with_store(&r, RunKind::Build, piece(), json!({ "value": { "ok": true } }), 200).await;
    r.starter.start(req).unwrap();
    assert_eq!(end_of(&mut s.ends).await.outcome, RunOutcome::PrOpened);
    assert_eq!(
        changes(&seen),
        vec![json!({ "token": "tok", "branch": BRANCH, "prUrl": "https://github.com/o/p/pull/7", "prNumber": 7 })]
    );
}

#[tokio::test]
async fn a_fix_on_the_pieces_existing_change_tells_the_store_nothing() {
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    let repo = r.root.join("repo");
    for args in [
        &["checkout", "-q", "-b", "pushed-branch"][..],
        &["push", "-q", "origin", "pushed-branch"],
        &["checkout", "-q", "main"],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let mut p = piece();
    p["branch"] = json!("pushed-branch");
    p["prUrl"] = json!("https://github.com/o/p/pull/7");
    p["prNumber"] = json!(7);
    let (req, mut s, seen) = request_with_store(&r, RunKind::Fix, p, json!({ "value": { "ok": true } }), 200).await;
    r.starter.start(req).unwrap();
    assert_eq!(end_of(&mut s.ends).await.outcome, RunOutcome::PrOpened);
    assert!(changes(&seen).is_empty(), "{:?}", seen.lock().unwrap());
}

#[tokio::test]
async fn a_refused_run_change_does_not_end_the_run() {
    let r = rig(&[COMMIT], &answer("pr_opened", ""));
    let reply = json!({ "error": { "code": "stale_run", "message": "gone" } });
    let (req, mut s, seen) = request_with_store(&r, RunKind::Build, piece(), reply, 409).await;
    r.starter.start(req).unwrap();
    assert_eq!(end_of(&mut s.ends).await.outcome, RunOutcome::PrOpened);
    assert_eq!(changes(&seen).len(), 1);
}
