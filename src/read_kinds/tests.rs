//! The arguments are those of `shared/cloudCommands.ts` (`read_file { path }`, `issues_preview { label?, milestone?,
//! anyone? }`); the answers are the shapes of `shared/issueImport.ts`'s `IssuesPreview` and `readRepoFile`.
use axum::{Json, Router, routing::post};
use serde_json::json;

use super::*;
use crate::test_support::repo_with_origin;

fn reading(repo_path: &str, base: &str) -> Reading {
    Reading {
        client: Arc::new(Client::new(base)),
        repo: 7,
        token: "tok".into(),
        project: ProjectConfig {
            repo_path: repo_path.into(),
            base_branch: "main".into(),
            github_repo: "o/r".into(),
            ..Default::default()
        },
        gh: "gh".into(),
    }
}

#[tokio::test]
async fn read_file_answers_the_markdown_from_the_base_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(tmp.path());
    let cx = reading(repo.to_str().unwrap(), "http://127.0.0.1:1");
    let got = read("read_file", &json!({ "path": "README.md" }), &cx).await.unwrap();
    assert_eq!(
        got,
        json!({ "path": "README.md", "ref": "origin/main", "content": "hello" })
    );
}

#[tokio::test]
async fn read_file_refuses_what_is_not_a_markdown_file_in_the_repo() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(tmp.path());
    let cx = reading(repo.to_str().unwrap(), "http://127.0.0.1:1");
    for path in [
        "../secret.md",
        "a/../b.md",
        "notes.txt",
        "a;b.md",
        ".md",
        "docs\\a.md",
        "",
    ] {
        let got = read("read_file", &json!({ "path": path }), &cx).await;
        assert_eq!(
            got,
            Err(ReadError::Refused(
                "Only Markdown files in the repo can be read.".into()
            )),
            "{path}"
        );
    }
    assert_eq!(
        read("read_file", &json!({ "path": "docs/nope.md" }), &cx).await,
        Err(ReadError::Refused("docs/nope.md isn't on origin/main.".into()))
    );
    assert_eq!(
        read("read_file", &json!({ "path": "A B/c-d_e.MD" }), &cx).await,
        Err(ReadError::Refused("A B/c-d_e.MD isn't on origin/main.".into())),
        "a path of that shape is read, so it is the file that is missing"
    );
}

#[cfg(unix)]
mod issues {
    use super::*;
    use crate::test_support::write_executable;

    const ISSUES: &str = r#"[
      { "number": 7, "title": "Seen already", "html_url": "https://github.com/o/r/issues/7", "user": { "login": "ana" }, "author_association": "OWNER" },
      { "number": 8, "title": "From a member", "html_url": "https://github.com/o/r/issues/8", "user": { "login": "bo" }, "author_association": "MEMBER" },
      { "number": 9, "title": "From a stranger", "html_url": "https://github.com/o/r/issues/9", "user": { "login": "cy" }, "author_association": "NONE" },
      { "number": 10, "title": "A pull request", "html_url": "https://github.com/o/r/pull/10", "user": { "login": "bo" }, "author_association": "MEMBER", "pull_request": {} }
    ]"#;

    /// A store whose drafts name issue #7 and hold two waiting plan requests.
    async fn store() -> String {
        let app = Router::new().route(
            "/v1/repos/{id}/ops",
            post(|Json(body): Json<serde_json::Value>| async move {
                let value = match body["op"].as_str() {
                    Some("drafts") => json!([
                        { "id": "d1", "status": "needs_person", "detail": "imported",
                          "issue": { "provider": "github", "key": "#7", "url": "u" } },
                        { "id": "d2", "status": "needs_person", "detail": "imported" },
                        { "id": "d3", "status": "needs_person", "detail": "awaiting_approval" }]),
                    _ => serde_json::Value::Null,
                };
                Json(json!({ "value": value }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        base
    }

    /// A `gh` that logs its arguments and answers the milestones and issues endpoints.
    fn gh(dir: &std::path::Path, fail: bool) -> std::path::PathBuf {
        let path = dir.join("gh");
        let script = if fail {
            "#!/bin/sh\necho 'gh: HTTP 401: Bad credentials' >&2\necho 'second line' >&2\nexit 1\n".to_string()
        } else {
            format!(
                "#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/calls.txt\"\ncase \"$*\" in\n*milestones*) echo '[{{\"number\":3,\"title\":\"v1\"}}]';;\n*) cat <<'EOF'\n{ISSUES}\nEOF\n;;\nesac\n"
            )
        };
        write_executable(&path, &script);
        path
    }

    async fn cx(dir: &std::path::Path, fail: bool) -> Reading {
        let mut cx = reading(dir.to_str().unwrap(), &store().await);
        cx.gh = gh(dir, fail);
        cx
    }

    #[tokio::test]
    async fn show_matches_tags_each_issue_as_an_import_would_treat_it() {
        let tmp = tempfile::tempdir().unwrap();
        let cx = cx(tmp.path(), false).await;
        let got = read("issues_preview", &json!({ "label": " bug " }), &cx).await.unwrap();
        assert_eq!(
            got,
            json!({ "issues": [
                { "key": "#7", "title": "Seen already", "url": "https://github.com/o/r/issues/7", "author": "ana", "comes": "seen" },
                { "key": "#8", "title": "From a member", "url": "https://github.com/o/r/issues/8", "author": "bo", "comes": "yes" },
                { "key": "#9", "title": "From a stranger", "url": "https://github.com/o/r/issues/9", "author": "cy", "comes": "untrusted" }],
                "waiting": 2 })
        );
        let calls = std::fs::read_to_string(tmp.path().join("calls.txt")).unwrap();
        assert!(
            calls.contains("api -X GET repos/o/r/issues -f state=open -f per_page=50 -f labels=bug"),
            "{calls}"
        );
        let anyone = read("issues_preview", &json!({ "label": "bug", "anyone": true }), &cx)
            .await
            .unwrap();
        assert_eq!(anyone["issues"][2]["comes"], "yes");
        assert_eq!(anyone["issues"][0]["comes"], "seen");
    }

    #[tokio::test]
    async fn a_milestone_is_looked_up_by_its_title_and_a_missing_one_is_said() {
        let tmp = tempfile::tempdir().unwrap();
        let cx = cx(tmp.path(), false).await;
        read("issues_preview", &json!({ "milestone": "v1" }), &cx)
            .await
            .unwrap();
        let calls = std::fs::read_to_string(tmp.path().join("calls.txt")).unwrap();
        assert!(calls.contains("-f milestone=3"), "{calls}");
        let missing = read("issues_preview", &json!({ "milestone": "v9" }), &cx)
            .await
            .unwrap();
        assert_eq!(missing, json!({ "issues": [], "waiting": 2, "milestoneMissing": true }));
    }

    #[tokio::test]
    async fn a_filter_is_needed_and_a_failed_gh_is_its_first_line() {
        let tmp = tempfile::tempdir().unwrap();
        let cx = cx(tmp.path(), false).await;
        for args in [
            json!({}),
            json!({ "label": "  ", "milestone": "" }),
            json!({ "anyone": true }),
        ] {
            assert_eq!(
                read("issues_preview", &args, &cx).await,
                Err(ReadError::Refused("issues needs a label or a milestone".into()))
            );
        }
        let broken = self::cx(tmp.path(), true).await;
        assert_eq!(
            read("issues_preview", &json!({ "label": "bug" }), &broken).await,
            Err(ReadError::Failed(
                "Couldn't read GitHub issues: gh: HTTP 401: Bad credentials".into()
            ))
        );
    }
}
