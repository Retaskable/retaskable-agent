use std::path::Path;

use serde_json::{Value, json};

use super::*;
use crate::test_support::{git, repo_with_origin, run};

fn user(text: &str, cwd: &str, branch: &str) -> Value {
    json!({ "type": "user", "cwd": cwd, "gitBranch": branch, "message": { "role": "user", "content": text } })
}

fn assistant(blocks: Value) -> Value {
    json!({ "type": "assistant", "message": { "role": "assistant", "content": blocks } })
}

fn write(root: &Path, project: &str, id: &str, events: &[Value]) -> PathBuf {
    let dir = root.join(project);
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{id}.jsonl"));
    let text: Vec<String> = events.iter().map(Value::to_string).collect();
    std::fs::write(&file, text.join("\n")).unwrap();
    file
}

#[test]
fn a_person_text_is_theirs_alone() {
    let said = |ev: Value| person_text(&ev);
    assert_eq!(said(user("fix it", "/w", "b")), "fix it");
    // Claude Code's own wrappers are cut out, whatever surrounds them.
    assert_eq!(
        said(user(
            "a <system-reminder>noise</system-reminder> b<ide_selection>x</ide_selection>",
            "/w",
            "b"
        )),
        "a  b"
    );
    assert_eq!(
        said(user("<command-name>/clear</command-name>real words", "/w", "b")),
        "real words"
    );
    // An opening tag with no end, or another tag, stays: it is the person's.
    assert_eq!(
        said(user("use <b>bold</b> and <system-reminder>", "/w", "b")),
        "use <b>bold</b> and <system-reminder>"
    );
    // Not the person's: a tool's result, a subagent's line, a meta line, a task's notice.
    let tool = json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "content": "ok" }] } });
    let side = json!({ "type": "user", "isSidechain": true, "message": { "content": "hi" } });
    let meta = json!({ "type": "user", "isMeta": true, "message": { "content": "hi" } });
    let note = json!({ "type": "user", "message": { "content": "<task-notification><summary>Monitor ended</summary></task-notification>" } });
    let kind = json!({ "type": "user", "origin": { "kind": "task-notification" }, "message": { "content": "done" } });
    for ev in [tool, side, meta, note, kind, assistant(json!([]))] {
        assert_eq!(said(ev.clone()), "", "{ev}");
    }
    // A list of text blocks is joined.
    let blocks = json!({ "type": "user", "message": { "content": [{ "type": "text", "text": "one" }, { "type": "text", "text": "two" }] } });
    assert_eq!(said(blocks), "one\ntwo");
}

#[test]
fn the_list_is_the_persons_sessions_newest_first_with_titles_branches_and_dirty_folders() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("projects");
    let repo = repo_with_origin(tmp.path());
    let folder = repo.to_string_lossy().to_string();
    std::fs::write(repo.join("wip.txt"), "x").unwrap();
    let lanes = tmp.path().join("lanes");
    let lane = lanes.join("lane-1").to_string_lossy().to_string();

    write(
        &root,
        "a",
        "old",
        &[user("The first thing to do\nand more", &folder, "feature/a")],
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    write(
        &root,
        "b",
        "titled",
        &[
            user("hello", &folder, "main"),
            json!({ "type": "ai-title", "aiTitle": "Claude's title" }),
        ],
    );
    // None of the person's messages, and one in a lane: neither is listed.
    write(
        &root,
        "c",
        "silent",
        &[assistant(json!([{ "type": "text", "text": "hi" }]))],
    );
    write(&root, "d", "lane", &[user("a lane's", &lane, "retaskable/x")]);
    std::fs::write(root.join("a").join("notes.txt"), "not a session").unwrap();

    let sessions = list(&root, &[lanes]);
    let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["titled", "old"]);
    assert_eq!(sessions[0].title, "Claude's title");
    assert_eq!(sessions[1].title, "The first thing to do");
    assert_eq!(sessions[1].branch.as_deref(), Some("feature/a"));
    assert_eq!(sessions[1].folder, folder);
    assert!(sessions.iter().all(|s| s.uncommitted), "{sessions:?}");
    let json = sessions[1].to_json();
    assert_eq!(json["branch"], "feature/a");
    assert_eq!(json["uncommitted"], true);
    assert!(json["at"].as_u64().unwrap() > 0);

    // Clean, and a folder that's gone: neither is "uncommitted"; a missing root is an empty list.
    run(git(&repo).args(["add", "wip.txt"]));
    run(git(&repo).args(["commit", "--quiet", "-m", "wip"]));
    write(&root, "e", "gone", &[user("somewhere else", "/no/such/folder", "")]);
    let sessions = list(&root, &[]);
    assert!(sessions.iter().filter(|s| s.id != "lane").all(|s| !s.uncommitted));
    assert_eq!(sessions.iter().find(|s| s.id == "gone").unwrap().branch, None);
    assert!(list(&tmp.path().join("nothing"), &[]).is_empty());
}

#[test]
fn the_title_and_folder_come_from_the_end_of_a_long_file() {
    let tmp = tempfile::tempdir().unwrap();
    let mut events = vec![user("start here", "/first", "old-branch")];
    // Enough in the middle that the end is read apart from the start.
    for _ in 0..400 {
        events.push(assistant(json!([{ "type": "text", "text": "x".repeat(1000) }])));
    }
    events.push(json!({ "type": "ai-title", "aiTitle": "Later title" }));
    events.push(json!({ "type": "user", "cwd": "/second", "gitBranch": "new-branch", "message": { "content": [] } }));
    let file = write(tmp.path(), "p", "long", &events);
    let s = describe(&file, &[]).unwrap();
    assert_eq!(
        (s.title.as_str(), s.folder.as_str(), s.branch.as_deref()),
        ("Later title", "/second", Some("new-branch"))
    );
}

#[test]
fn a_session_that_cant_be_continued_says_why() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write(root, "p", "silent", &[assistant(json!([]))]);
    assert_eq!(
        continuing(root, "nope").unwrap_err(),
        "That Claude Code session isn't on this computer any more."
    );
    assert_eq!(
        continuing(root, "silent").unwrap_err(),
        "That Claude Code session has none of your messages to continue from."
    );
    // An id that isn't an id is never made a file name.
    assert!(session_file(root, "../p/silent").is_none());
    write(root, "p", "fine", &[user("hello there", "/w", "b")]);
    let c = continuing(root, "fine").unwrap();
    assert_eq!(
        (c.title.as_str(), c.folder.as_str(), c.branch.as_deref()),
        ("hello there", "/w", Some("b"))
    );
}

fn talk() -> Vec<Value> {
    vec![
        user("first ask", "/w", "b"),
        assistant(json!([
            { "type": "text", "text": "  On it.  " },
            { "type": "tool_use", "name": "Edit", "input": { "file_path": "src/a.ts" } },
            { "type": "tool_use", "name": "Bash", "input": { "command": "npm\n test" } },
        ])),
        json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "content": "lots of output" }] } }),
        json!({ "type": "assistant", "isSidechain": true, "message": { "content": [{ "type": "text", "text": "a subagent" }] } }),
        user("second ask", "/w", "b"),
    ]
}

#[test]
fn the_handover_is_the_conversation_the_folder_and_what_git_says_then_the_message() {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("work");
    std::fs::create_dir_all(&folder).unwrap();
    let file = write(tmp.path(), "p", "s", &talk());
    let c = Continued {
        session_id: "s".into(),
        file,
        title: "t".into(),
        folder: folder.to_string_lossy().into(),
        branch: Some("feature/x".into()),
    };
    let text = handover(&c, "now do this").unwrap();
    let want = format!(
        "This continues a Claude Code session the person had on this computer. The latest part of its conversation:\n\n\
         Person: first ask\nClaude: On it.\n- Edit: src/a.ts\n- Bash: npm test\nPerson: second ask\n\n\
         It worked in {} on branch feature/x. `git status --short` there: clean.\n\nThe person's message now:\nnow do this",
        folder.display()
    );
    assert_eq!(text, want);
    // A file that is gone is no handover.
    std::fs::remove_file(&c.file).unwrap();
    assert_eq!(handover(&c, "x"), None);
}

#[test]
fn a_long_conversation_keeps_its_newest_and_says_it_was_cut() {
    let tmp = tempfile::tempdir().unwrap();
    let file = write(tmp.path(), "p", "s", &talk());
    let c = Continued {
        session_id: "s".into(),
        file,
        title: "t".into(),
        folder: "/gone".into(),
        branch: None,
    };
    // Room for the last two lines only.
    let cut = handover::handover_chars(&c, "m", 40).unwrap();
    assert!(
        cut.contains("\n\n(Earlier conversation left out.)\n- Bash: npm test\nPerson: second ask\n\n"),
        "{cut}"
    );
    assert!(!cut.contains("first ask"));
    assert!(cut.contains("It worked in /gone. `git status --short` there: clean."));
    // The newest message alone is longer than the whole: its end is kept.
    let one = handover::handover_chars(&c, "m", 8).unwrap();
    assert!(one.contains("(Earlier conversation left out.)\n…ond ask\n"), "{one}");
}

#[test]
fn a_dirty_folder_puts_its_status_in_the_handover() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(tmp.path());
    std::fs::write(repo.join("new.txt"), "x").unwrap();
    let file = write(tmp.path(), "p", "s", &talk());
    let c = Continued {
        session_id: "s".into(),
        file,
        title: "t".into(),
        folder: repo.to_string_lossy().into(),
        branch: None,
    };
    let text = handover(&c, "go").unwrap();
    assert!(text.contains("`git status --short` there: \n?? new.txt\n"), "{text}");
}

#[test]
fn the_projects_root_is_claudes_config_folder() {
    let env = Env::from_pairs([("CLAUDE_CONFIG_DIR", "/cfg")]);
    assert_eq!(projects_root(&env), Path::new("/cfg").join("projects"));
    let env = Env::from_pairs([("HOME", "/home/ada")]);
    assert_eq!(
        projects_root(&env),
        Path::new("/home/ada").join(".claude").join("projects")
    );
}

/// `repo` (with its origin) and a clone of it on `branch`, one commit ahead of main.
fn clone_on(tmp: &Path, repo: &Path, branch: &str) -> PathBuf {
    let other = tmp.join("clone");
    run(std::process::Command::new("git")
        .args(["clone", "--quiet"])
        .arg(tmp.join("origin.git"))
        .arg(&other));
    run(git(&other).args(["config", "user.email", "t@example.com"]));
    run(git(&other).args(["config", "user.name", "T"]));
    run(git(&other).args(["checkout", "-b", branch]));
    std::fs::write(other.join("work.txt"), "from the person's claude code").unwrap();
    run(git(&other).args(["add", "work.txt"]));
    run(git(&other).args(["commit", "--quiet", "-m", "work"]));
    assert!(repo.is_dir());
    other
}

#[test]
fn the_branch_starts_at_the_tip_of_the_folders_and_is_pushed() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(tmp.path());
    let other = clone_on(tmp.path(), &repo, "feature/x");
    branch_at_tip(&repo, "main", &other, Some("feature/x"), "retaskable/expert-x-x").unwrap();
    let tip = |dir: &Path, r: &str| {
        let out = git(dir).args(["rev-parse", r]).output().unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let want = tip(&other, "feature/x");
    assert_eq!(tip(&repo, "refs/heads/retaskable/expert-x-x"), want);
    assert_eq!(
        tip(&tmp.path().join("origin.git"), "refs/heads/retaskable/expert-x-x"),
        want
    );
    // A worktree of the project's own repository counts too.
    let tree = tmp.path().join("tree");
    run(git(&repo).args(["worktree", "add", "--quiet", "-b", "wt", tree.to_str().unwrap()]));
    branch_at_tip(&repo, "main", &tree, Some("wt"), "retaskable/expert-y-y").unwrap();
}

#[test]
fn a_folder_that_isnt_a_branch_of_this_project_starts_from_the_base() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = repo_with_origin(tmp.path());
    let other = clone_on(tmp.path(), &repo, "feature/x");
    let why =
        |folder: &Path, branch: Option<&str>| branch_at_tip(&repo, "main", folder, branch, "retaskable/n").unwrap_err();
    assert_eq!(why(&other, None), "its folder wasn't on a branch");
    assert_eq!(why(&other, Some("HEAD")), "its folder wasn't on a branch");
    assert_eq!(why(&other, Some("main")), "it worked on main itself");
    assert_eq!(why(&tmp.path().join("gone"), Some("feature/x")), "its folder is gone");
    // Another project's checkout.
    let stranger = tempfile::tempdir().unwrap();
    let elsewhere = repo_with_origin(stranger.path());
    assert_eq!(
        why(&elsewhere, Some("main2")),
        "its folder isn't a checkout of this project"
    );
    // A branch name that isn't one to put in a refspec is refused before git sees it.
    for bad in ["-x", "a:b", "a..b", "has space", "x@{1}"] {
        assert!(why(&other, Some(bad)).contains("isn't one to copy"), "{bad}");
    }
    // The branch isn't in the folder after all: git's reason is the reason.
    assert!(why(&other, Some("no-such")).starts_with("its branch couldn't be copied ("));
    // Nothing was made on the way.
    let out = git(&repo).args(["branch", "--list", "retaskable/n"]).output().unwrap();
    assert!(out.stdout.is_empty());
}
