use std::sync::atomic::{AtomicUsize, Ordering};

use super::files::pick_backlog;
use super::*;

pub(crate) const LEAKY: &str = "ghp_abcdefghijklmnop0123456789";

type Jobs = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

struct Rig {
    dir: tempfile::TempDir,
    add: Arc<AddProject>,
    clones: Arc<AtomicUsize>,
    restarts: Arc<AtomicUsize>,
    /// Jobs the add handed over: run by hand with [`Rig::run`], so a test sees the "working" state first.
    jobs: Jobs,
}

fn git(dir: &Path, args: &[&str]) {
    let out = crate::git_bin::command(dir).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo the way a clone leaves one: a backlog file, a workflow, on `trunk`.
pub(crate) fn make_repo(dest: &Path, origin: Option<&str>) {
    std::fs::create_dir_all(dest.join("docs/specs/engines")).unwrap();
    std::fs::create_dir_all(dest.join(".github/workflows")).unwrap();
    std::fs::write(dest.join("docs/specs/engines/10-build-order.md"), "# Build order\n").unwrap();
    std::fs::write(dest.join(".github/workflows/ci.yml"), "on: push\n").unwrap();
    git(dest, &["init", "-q", "-b", "trunk"]);
    if let Some(url) = origin {
        git(dest, &["remote", "add", "origin", url]);
    }
    git(dest, &["add", "-A"]);
    git(
        dest,
        &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "x"],
    );
}

impl Rig {
    fn new(clone_fails: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clones = Arc::new(AtomicUsize::new(0));
        let restarts = Arc::new(AtomicUsize::new(0));
        let jobs: Jobs = Arc::default();
        let (c, r, j) = (clones.clone(), restarts.clone(), jobs.clone());
        let add = AddProject::with(
            dir.path().join("data"),
            Env::from_pairs([("GH_TOKEN", LEAKY)]),
            RepoPicker::with(
                Box::new(|| Ok(vec!["me".into(), "Org".into()])),
                Box::new(|owner| {
                    Ok(vec![RepoSummary {
                        name_with_owner: format!("{owner}/app"),
                        description: None,
                        visibility: "PRIVATE".into(),
                        is_fork: false,
                    }])
                }),
                Box::new(std::time::Instant::now),
            ),
            Box::new(move |repo, dest| {
                c.fetch_add(1, Ordering::SeqCst);
                if clone_fails {
                    // gh's own words, with the token a bad URL would echo.
                    bail!(
                        "Couldn't clone {repo}: remote: Repository not found. https://x-access-token:{LEAKY}@github.com/{repo}"
                    );
                }
                make_repo(dest, None);
                Ok(())
            }),
            Box::new(|_| Some("trunk".to_string())),
            Box::new(move |job| j.lock().unwrap().push(job)),
            Box::new(move || {
                r.fetch_add(1, Ordering::SeqCst);
            }),
        );
        // The agent's data folder, with a config that already lists one project, so clones go beside it.
        let me = Self {
            dir,
            add,
            clones,
            restarts,
            jobs,
        };
        std::fs::create_dir_all(me.data()).unwrap();
        me
    }

    fn data(&self) -> PathBuf {
        self.dir.path().join("data")
    }

    fn code(&self) -> PathBuf {
        self.dir.path().join("code")
    }

    /// `config.json` with one project, at `code/first`.
    fn seed_first(&self) {
        let first = self.code().join("first");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::write(
            self.data().join("config.json"),
            json!({ "version": 2, "port": 4701, "projects": [{
                "id": "first", "repoPath": first, "githubRepo": "me/first", "baseBranch": "main"
            }] })
            .to_string(),
        )
        .unwrap();
    }

    /// Runs what the add handed over.
    fn run(&self) {
        let jobs: Vec<_> = self.jobs.lock().unwrap().drain(..).collect();
        for job in jobs {
            job();
        }
    }

    fn config(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.data().join("config.json")).unwrap()).unwrap()
    }
}

#[test]
fn an_add_clones_beside_the_last_project_and_writes_the_entry_with_its_defaults() {
    let rig = Rig::new(false);
    rig.seed_first();
    rig.add.start("Org/app").unwrap();
    assert!(
        matches!(rig.add.status(), Status::Working { .. }),
        "working until the job runs"
    );
    rig.run();
    let folder = rig.code().join("app");
    let Status::Done { id, repo, .. } = rig.add.status() else {
        panic!("{:?}", rig.add.status())
    };
    assert_eq!((id.as_str(), repo.as_str()), ("app", "Org/app"));
    let cfg = rig.config();
    assert_eq!(cfg["port"], 4701, "the file's other settings are kept");
    let entry = &cfg["projects"][1];
    assert_eq!(entry["id"], "app");
    assert_eq!(entry["repoPath"], folder.to_string_lossy().as_ref());
    assert_eq!(entry["githubRepo"], "Org/app");
    assert_eq!(entry["baseBranch"], "trunk", "the repo's default branch, never empty");
    assert_eq!(entry["backlogPath"], "docs/specs/engines/10-build-order.md");
    assert_eq!(
        entry["worktreesDir"],
        rig.code()
            .join(".retaskable-worktrees")
            .join("app")
            .to_string_lossy()
            .as_ref()
    );
    assert_eq!(entry["requireCi"], true, "it has a workflow");
    assert_eq!(entry["deploy"]["mergesTo"], "production");
    assert_eq!(cfg["projects"].as_array().unwrap().len(), 2);
    // The new entry reads back as a project config the agent loads.
    let loaded = config::load(&rig.data()).unwrap();
    assert_eq!(loaded.projects[1].base_branch, "trunk");
    assert_eq!(
        rig.restarts.load(Ordering::SeqCst),
        1,
        "restarted once, to pick the project up"
    );
}

#[test]
fn the_new_project_is_picked_up_by_the_next_start_already_paused_with_the_variable_unset() {
    let rig = Rig::new(false);
    rig.seed_first();
    rig.add.start("Org/app").unwrap();
    rig.run();
    // What the next start does: load the projects, then apply the start-paused record.
    let ids: Vec<String> = config::load(&rig.data())
        .unwrap()
        .projects
        .iter()
        .map(|p| p.id_or_derived())
        .collect();
    assert_eq!(ids, ["first", "app"]);
    let paused = crate::start_paused::apply(&rig.data(), &Env::empty(), &ids, |_, _| {});
    assert_eq!(
        paused,
        ["app"],
        "only the new one, and with RETASKABLE_START_PAUSED unset"
    );
}

#[test]
fn a_repo_that_is_added_already_is_refused_whatever_its_case() {
    let rig = Rig::new(false);
    rig.seed_first();
    for text in ["me/first", "ME/First", "https://github.com/me/first.git"] {
        let got = rig.add.start(text);
        assert!(
            matches!(got, Err(Refusal::Conflict(m)) if m.contains("added already")),
            "{text}"
        );
    }
    assert_eq!(rig.clones.load(Ordering::SeqCst), 0);
    assert_eq!(rig.add.status(), Status::Idle);
}

#[test]
fn only_one_add_runs_at_a_time() {
    let rig = Rig::new(false);
    rig.seed_first();
    rig.add.start("Org/app").unwrap();
    let got = rig.add.start("Org/other");
    assert!(matches!(got, Err(Refusal::Conflict(m)) if m.contains("Still adding Org/app")));
    rig.run();
    rig.add.start("Org/other").unwrap();
    rig.run();
    assert_eq!(config::load(&rig.data()).unwrap().projects.len(), 3);
}

#[test]
fn a_bad_name_is_refused_before_anything_runs() {
    let rig = Rig::new(false);
    for text in ["", "nope", "a/b/c", "../x", "a/..", "a b/c", "a/b; rm -rf", "-"] {
        assert!(matches!(rig.add.start(text), Err(Refusal::Bad(_))), "{text:?}");
    }
    assert!(rig.jobs.lock().unwrap().is_empty());
    assert_eq!(rig.clones.load(Ordering::SeqCst), 0);
    assert_eq!(parse_repo(" https://github.com/Org/app.git "), Some("Org/app".into()));
    assert_eq!(parse_repo("Org/app"), Some("Org/app".into()));
}

#[test]
fn a_failed_clone_says_why_in_words_leaves_nothing_behind_and_never_shows_the_token() {
    let rig = Rig::new(true);
    rig.seed_first();
    let before = rig.config();
    rig.add.start("Org/app").unwrap();
    rig.run();
    let Status::Failed { message, .. } = rig.add.status() else {
        panic!("{:?}", rig.add.status())
    };
    assert!(message.contains("Repository not found"), "{message}");
    assert!(!message.contains(LEAKY) && !message.contains("ghp_"), "{message}");
    assert!(!rig.add.status().json().to_string().contains(LEAKY));
    assert_eq!(rig.config(), before, "config.json is as it was");
    assert!(!rig.data().join("start-paused.json").exists(), "nothing held");
    assert_eq!(rig.restarts.load(Ordering::SeqCst), 0);
    // Not stuck: another add can start.
    assert!(rig.add.start("Org/app").is_ok());
}

#[test]
fn a_copy_already_on_this_computer_is_used_not_cloned_again() {
    let rig = Rig::new(false);
    rig.seed_first();
    make_repo(&rig.code().join("my-app-copy"), Some("https://github.com/Org/app.git"));
    rig.add.start("org/app").unwrap();
    rig.run();
    assert_eq!(rig.clones.load(Ordering::SeqCst), 0);
    let entry = &rig.config()["projects"][1];
    assert_eq!(entry["id"], "my-app-copy");
    assert!(entry["repoPath"].as_str().unwrap().ends_with("my-app-copy"));
}

#[test]
fn a_folder_that_is_in_the_way_stops_it_in_words_and_is_left_alone() {
    let rig = Rig::new(false);
    rig.seed_first();
    std::fs::create_dir_all(rig.code().join("app")).unwrap();
    std::fs::write(rig.code().join("app").join("mine.txt"), "keep").unwrap();
    rig.add.start("Org/app").unwrap();
    rig.run();
    let Status::Failed { message, .. } = rig.add.status() else {
        panic!()
    };
    assert!(message.contains("already exists"), "{message}");
    assert!(rig.code().join("app").join("mine.txt").exists());
    assert_eq!(rig.clones.load(Ordering::SeqCst), 0);
}

#[test]
fn a_repo_with_no_markdown_or_no_known_branch_is_not_added_and_its_clone_goes() {
    let rig = Rig::new(false);
    rig.seed_first();
    let empty = AddProject::with(
        rig.data(),
        Env::empty(),
        RepoPicker::new(),
        Box::new(|_, dest| {
            std::fs::create_dir_all(dest).unwrap();
            std::fs::write(dest.join("x.txt"), "x").unwrap();
            git(dest, &["init", "-q", "-b", "trunk"]);
            git(dest, &["add", "-A"]);
            git(
                dest,
                &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "x"],
            );
            Ok(())
        }),
        Box::new(|_| Some("trunk".into())),
        Box::new(|job| job()),
        Box::new(|| {}),
    );
    empty.start("Org/plain").unwrap();
    let Status::Failed { message, .. } = empty.status() else {
        panic!()
    };
    assert!(message.contains("no Markdown file"), "{message}");
    assert!(!rig.code().join("plain").exists(), "the clone it made is removed");
    assert_eq!(config::load(&rig.data()).unwrap().projects.len(), 1);

    let no_branch = AddProject::with(
        rig.data(),
        Env::empty(),
        RepoPicker::new(),
        Box::new(|_, dest| {
            make_repo(dest, None);
            Ok(())
        }),
        Box::new(|_| None),
        Box::new(|job| job()),
        Box::new(|| {}),
    );
    no_branch.start("Org/nb").unwrap();
    let Status::Failed { message, .. } = no_branch.status() else {
        panic!()
    };
    assert!(message.contains("which branch"), "{message}");
    assert_eq!(config::load(&rig.data()).unwrap().projects.len(), 1);
}

#[test]
fn the_picker_lists_an_owners_repos_and_marks_the_ones_added_and_refuses_a_stranger() {
    let rig = Rig::new(false);
    rig.seed_first();
    assert_eq!(rig.add.owners(false).unwrap(), ["me", "Org"]);
    let rows = rig.add.repos("me", false).unwrap();
    assert_eq!((rows[0].0.name_with_owner.as_str(), rows[0].1), ("me/app", false));
    assert!(matches!(rig.add.repos("stranger", false), Err(Refusal::Bad(_))));
    // `me/first` is added: a listing that holds it says so.
    let listing = AddProject::with(
        rig.data(),
        Env::empty(),
        RepoPicker::with(
            Box::new(|| Ok(vec!["me".into()])),
            Box::new(|_| {
                Ok(vec![RepoSummary {
                    name_with_owner: "me/First".into(),
                    description: None,
                    visibility: "PUBLIC".into(),
                    is_fork: false,
                }])
            }),
            Box::new(std::time::Instant::now),
        ),
        Box::new(|_, _| Ok(())),
        Box::new(|_| None),
        Box::new(|_| {}),
        Box::new(|| {}),
    );
    assert!(listing.repos("me", false).unwrap()[0].1);
}

#[test]
fn clones_go_beside_the_last_project_else_the_work_volume_else_home() {
    let env = Env::from_pairs([("HOME", "/home/pat")]);
    let work = tempfile::tempdir().unwrap();
    let project = |path: &str| ProjectConfig {
        repo_path: path.into(),
        ..Default::default()
    };
    let gone = Path::new("/no/such/work");
    assert_eq!(
        clone_dir(&[project("/a/x"), project("/b/y")], &env, work.path()),
        Path::new("/b")
    );
    assert_eq!(clone_dir(&[], &env, work.path()), work.path());
    assert_eq!(clone_dir(&[], &env, gone), Path::new("/home/pat"));
    assert_eq!(
        clone_dir(&[], &Env::from_pairs([("USERPROFILE", "C:/Users/pat")]), gone),
        Path::new("C:/Users/pat")
    );
}

#[test]
fn the_backlog_is_a_known_name_then_one_named_like_a_roadmap_then_any_markdown() {
    let files = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        pick_backlog(&files(&["README.md", "docs/specs/build-order.md", "BACKLOG.md"])).as_deref(),
        Some("docs/specs/build-order.md")
    );
    assert_eq!(
        pick_backlog(&files(&["README.md", "docs/specs/engines/10-build-order.md"])).as_deref(),
        Some("docs/specs/engines/10-build-order.md")
    );
    assert_eq!(
        pick_backlog(&files(&["NOTES.md", "README.md"])).as_deref(),
        Some("NOTES.md")
    );
    assert_eq!(pick_backlog(&[]), None);
}

#[test]
fn a_second_project_with_the_same_folder_name_gets_its_own_id() {
    let taken = ProjectConfig {
        id: Some("app".into()),
        ..Default::default()
    };
    assert_eq!(
        files::unique_id(Path::new("/x/app"), std::slice::from_ref(&taken)),
        "app-2"
    );
    assert_eq!(files::unique_id(Path::new("/x/App.js"), &[taken]), "app-js");
}

#[test]
fn a_token_is_taken_out_of_any_text_by_its_shape_and_by_its_value() {
    let env = Env::from_pairs([("GITHUB_TOKEN", "plain-secret-value")]);
    let out = redact(
        &format!(
            "bad {LEAKY} and github_pat_11AAAAAAA_bbbbbbbbbb and plain-secret-value and https://x-access-token:zzz@github.com"
        ),
        &env,
    );
    assert_eq!(
        out,
        "bad [token] and [token] and [token] and https://[token]@github.com"
    );
}

#[test]
fn an_old_one_project_config_file_is_refused_not_rewritten() {
    let rig = Rig::new(false);
    let first = rig.code().join("only");
    std::fs::create_dir_all(&first).unwrap();
    let old = json!({ "repoPath": first, "githubRepo": "me/only", "baseBranch": "main" }).to_string();
    std::fs::write(rig.data().join("config.json"), &old).unwrap();
    rig.add.start("Org/app").unwrap();
    rig.run();
    let Status::Failed { message, .. } = rig.add.status() else {
        panic!()
    };
    assert!(message.contains("old one-project format"), "{message}");
    assert_eq!(std::fs::read_to_string(rig.data().join("config.json")).unwrap(), old);
}
