//! `flush_owed` trims and cuts a give-back's note and sends no `report` for a blank one.
use super::*;

/// A store that records every op and refuses `report` / `release` with a 500 while their flags are set.
async fn stub(fail_report: Arc<AtomicBool>, fail_release: Arc<AtomicBool>) -> (String, Asked) {
    use axum::http::StatusCode;
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let seen = asked.clone();
    let app = Router::new().route(
        "/v1/repos/{id}/ops",
        post(move |Json(body): Json<Value>| {
            let (seen, fail_report, fail_release) = (seen.clone(), fail_report.clone(), fail_release.clone());
            async move {
                let op = body["op"].as_str().unwrap_or("").to_string();
                if op == "settings_get" {
                    return (StatusCode::OK, Json(json!({ "value": null })));
                }
                seen.lock().unwrap().push((op.clone(), body["args"].clone()));
                let refuse = (op == "report" && fail_report.load(Ordering::SeqCst))
                    || (op == "release" && fail_release.load(Ordering::SeqCst));
                if refuse {
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "boom" })))
                } else {
                    (StatusCode::OK, Json(json!({ "value": [] })))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), asked)
}

async fn owing(note: &str, fr: &Arc<AtomicBool>, fl: &Arc<AtomicBool>) -> (Rig, Asked) {
    let (base, asked) = stub(fr.clone(), fl.clone()).await;
    let mut rig = rig(2, &[&base]);
    rig.lp(0).unreleased.insert("a".into(), ("lease-1".into(), note.into()));
    rig.go_live();
    rig.machine.pass().await;
    (rig, asked)
}

fn notes(asked: &Asked) -> Vec<String> {
    let seen = asked.lock().unwrap();
    let reports = seen.iter().filter(|(o, _)| o == "report");
    reports.map(|(_, a)| a["note"].as_str().unwrap().to_string()).collect()
}

fn owed(asked: &Asked) -> Vec<String> {
    ops(asked)
        .into_iter()
        .filter(|o| o == "report" || o == "release")
        .collect()
}

#[tokio::test]
async fn a_long_note_is_cut_at_4000_characters_then_released() {
    let off = Arc::new(AtomicBool::new(false));
    let (mut rig, asked) = owing(&"é".repeat(5000), &off, &off).await;
    assert_eq!(notes(&asked), vec!["é".repeat(4000)]);
    assert_eq!(owed(&asked), ["report", "release"]);
    assert!(rig.lp(0).unreleased.is_empty());
}

#[tokio::test]
async fn a_blank_note_sends_the_release_alone() {
    let off = Arc::new(AtomicBool::new(false));
    for note in ["", " \n\t "] {
        let (mut rig, asked) = owing(note, &off, &off).await;
        assert_eq!(owed(&asked), ["release"]);
        assert!(rig.lp(0).unreleased.is_empty());
    }
}

#[tokio::test]
async fn a_failed_report_keeps_the_entry_and_both_calls_go_next_pass() {
    let (fr, off) = (Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(false)));
    let (mut rig, asked) = owing("why", &fr, &off).await;
    assert_eq!(owed(&asked), ["report"]);
    assert_eq!(rig.lp(0).unreleased["a"].1, "why");
    fr.store(false, Ordering::SeqCst);
    rig.machine.pass().await;
    assert_eq!(owed(&asked), ["report", "report", "release"]);
    assert!(rig.lp(0).unreleased.is_empty());
}

#[tokio::test]
async fn a_refused_release_of_a_blank_note_goes_out_again_alone() {
    let (off, fl) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(true)));
    let (mut rig, asked) = owing("  ", &off, &fl).await;
    assert!(rig.lp(0).unreleased.contains_key("a"));
    fl.store(false, Ordering::SeqCst);
    rig.machine.pass().await;
    assert_eq!(owed(&asked), ["release", "release"]);
    assert!(rig.lp(0).unreleased.is_empty());
}
