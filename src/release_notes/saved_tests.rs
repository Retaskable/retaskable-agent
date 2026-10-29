use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode;
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};

use super::*;
use crate::test_support::{git, repo_with_origin, run};

fn note(to: &str, extra: &str) -> String {
    format!("---\nfrom: 2026-09-01\nto: \"{to}\"\n---\n\n# Notes {to}\n{extra}")
}

fn file(name: &str) -> ReleaseFile {
    ReleaseFile {
        path: format!("docs/releases/{name}"),
        content: note("2026-09-30", ""),
    }
}

// release_of_file

#[test]
fn a_note_file_becomes_a_row_only_with_its_dates_a_usable_id_and_a_body_within_the_cap() {
    let row = release_of_file(&file("2026-09-30.md")).unwrap();
    assert_eq!(
        (row.id.as_str(), row.from.as_str(), row.to.as_str(), row.title.as_str()),
        ("2026-09-30", "2026-09-01", "2026-09-30", "Notes 2026-09-30")
    );
    assert_eq!(row.created_at, 1_790_726_400_000, "the start of its to day, UTC");

    let no_front = ReleaseFile {
        path: "docs/releases/x.md".to_string(),
        content: "# no front matter\n".to_string(),
    };
    assert_eq!(release_of_file(&no_front), None);
    assert_eq!(release_of_file(&file("a b.md")), None, "an id the store won't take");
    let big = ReleaseFile {
        path: "docs/releases/big.md".to_string(),
        content: note("2026-09-30", &"x".repeat(RELEASE_BODY_MAX_BYTES)),
    };
    assert_eq!(release_of_file(&big), None, "over the cap");
}

#[test]
fn a_note_with_no_heading_is_titled_by_its_id() {
    let f = ReleaseFile {
        path: "docs/releases/v1.2.md".to_string(),
        content: "---\nfrom: 2026-09-01\nto: 2026-09-30\n---\nJust words.\n".to_string(),
    };
    assert_eq!(release_of_file(&f).unwrap().title, "v1.2");
}

// read_release_files

#[tokio::test]
async fn read_release_files_reads_the_markdown_in_docs_releases_at_the_ref() {
    let root = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(root.path());
    let git_bin = PathBuf::from("git");
    assert!(
        read_release_files(&git_bin, &repo, "HEAD").await.is_empty(),
        "no folder yet"
    );

    let dir = repo.join("docs").join("releases");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("2026-09-30.md"), note("2026-09-30", "")).unwrap();
    std::fs::write(dir.join("notes.txt"), "not a note").unwrap();
    run(git(&repo).args(["add", "docs"]));
    run(git(&repo).args(["commit", "--quiet", "-m", "notes"]));

    let files = read_release_files(&git_bin, &repo, "HEAD").await;
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].path, "docs/releases/2026-09-30.md");
    assert!(files[0].content.contains("# Notes 2026-09-30"));
}

// SavedReleases

#[derive(Default)]
struct Fake {
    ops: Vec<Value>,
    refuse_offer: bool,
}

async fn handle(State(fake): State<Arc<Mutex<Fake>>>, Json(req): Json<Value>) -> (StatusCode, Json<Value>) {
    let mut fake = fake.lock().unwrap();
    fake.ops.push(req.clone());
    let row = json!({ "id": "2026-09-30", "title": "Notes", "from": "2026-09-01", "to": "2026-09-30",
                      "createdAt": 1, "by": "jo" });
    match (req["op"].as_str(), req["args"]["id"].as_str()) {
        (Some("releases_offer"), _) if fake.refuse_offer => (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "bad_request", "message": "body is over 128 KB" })),
        ),
        (Some("releases_offer"), _) => (StatusCode::OK, Json(json!({ "value": { "taken": ["2026-09-30"] } }))),
        (Some("releases_get"), Some("2026-09-30")) => {
            let mut one = row.clone();
            one["body"] = json!("# Notes");
            (StatusCode::OK, Json(json!({ "value": one })))
        }
        (Some("releases_get"), Some(_)) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "not_found", "message": "no such note" })),
        ),
        (Some("releases_get"), None) => (StatusCode::OK, Json(json!({ "value": [row] }))),
        _ => (StatusCode::BAD_REQUEST, Json(json!({ "error": "bad_request" }))),
    }
}

async fn fake_store(fake: Arc<Mutex<Fake>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = Router::new().route("/v1/repos/{id}/ops", post(handle)).with_state(fake);
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("http://{addr}")
}

fn saved<'a>(client: &'a Client, dir: &Path) -> SavedReleases<'a> {
    SavedReleases {
        client,
        repo_id: 1,
        token: Some("tok"),
        offered_file: offered_file(dir),
        machine: "desk".to_string(),
    }
}

fn ops(fake: &Arc<Mutex<Fake>>, op: &str) -> Vec<Value> {
    fake.lock()
        .unwrap()
        .ops
        .iter()
        .filter(|o| o["op"] == op)
        .cloned()
        .collect()
}

#[tokio::test]
async fn the_first_list_offers_the_repos_notes_once_and_then_reads_the_store() {
    let fake = Arc::new(Mutex::new(Fake::default()));
    let client = Client::new(fake_store(fake.clone()).await);
    let dir = tempfile::tempdir().unwrap();
    let releases = saved(&client, dir.path());
    let skip = ReleaseFile {
        path: "docs/releases/skip.md".to_string(),
        content: "nothing".to_string(),
    };

    let list = releases.list(async || vec![file("2026-09-30.md"), skip]).await.unwrap();
    assert_eq!(list[0].id, "2026-09-30");
    assert_eq!(list[0].body, None);
    let offers = ops(&fake, "releases_offer");
    assert_eq!(offers.len(), 1);
    assert_eq!(offers[0]["args"]["machine"], "desk");
    assert_eq!(
        offers[0]["args"]["releases"].as_array().unwrap().len(),
        1,
        "only a usable file"
    );
    assert_eq!(offers[0]["args"]["releases"][0]["createdAt"], 1_790_726_400_000_i64);

    releases
        .list(async || panic!("the files aren't read again once offered"))
        .await
        .unwrap();
    assert_eq!(ops(&fake, "releases_offer").len(), 1, "offered once");
}

#[tokio::test]
async fn a_store_that_cant_be_reached_gives_none_and_the_offer_is_tried_again() {
    let dir = tempfile::tempdir().unwrap();
    let down = Client::new("http://127.0.0.1:9");
    assert_eq!(
        saved(&down, dir.path())
            .list(async || vec![file("2026-09-30.md")])
            .await,
        None
    );
    assert!(!offered_file(dir.path()).exists());

    let fake = Arc::new(Mutex::new(Fake::default()));
    let client = Client::new(fake_store(fake.clone()).await);
    let list = saved(&client, dir.path())
        .list(async || vec![file("2026-09-30.md")])
        .await;
    assert_eq!(list.unwrap().len(), 1);
    assert!(offered_file(dir.path()).exists());
}

#[tokio::test]
async fn a_store_that_refuses_the_offer_is_not_asked_again() {
    let fake = Arc::new(Mutex::new(Fake {
        refuse_offer: true,
        ..Fake::default()
    }));
    let client = Client::new(fake_store(fake.clone()).await);
    let dir = tempfile::tempdir().unwrap();
    let releases = saved(&client, dir.path());
    assert!(releases.list(async || vec![file("2026-09-30.md")]).await.is_some());
    releases.list(async || vec![file("2026-09-30.md")]).await.unwrap();
    assert_eq!(ops(&fake, "releases_offer").len(), 1);
}

#[tokio::test]
async fn nothing_to_offer_asks_the_store_nothing_but_is_remembered() {
    let fake = Arc::new(Mutex::new(Fake::default()));
    let client = Client::new(fake_store(fake.clone()).await);
    let dir = tempfile::tempdir().unwrap();
    saved(&client, dir.path()).list(async || Vec::new()).await.unwrap();
    assert!(ops(&fake, "releases_offer").is_empty());
    assert!(offered_file(dir.path()).exists());
}

#[tokio::test]
async fn get_reads_a_saved_note_with_its_markdown_and_a_missing_one_is_not_found() {
    let fake = Arc::new(Mutex::new(Fake::default()));
    let client = Client::new(fake_store(fake.clone()).await);
    let dir = tempfile::tempdir().unwrap();
    let releases = saved(&client, dir.path());
    assert_eq!(
        releases.get("2026-09-30").await.unwrap().body.as_deref(),
        Some("# Notes")
    );
    assert_eq!(releases.get("nope").await.unwrap_err().code(), Some("not_found"));
}
