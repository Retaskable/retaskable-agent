use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::backlog::ItemStatus;
use crate::config::RoleEngines;
use crate::orchestrator::driver::OutcomeSender;

const FINISHED: &str = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened","summary":"done"},"total_cost_usd":0.1,"session_id":"s"}"#;

struct Rig {
    _dir: tempfile::TempDir,
    root: PathBuf,
    starter: Arc<ClaudeStarter>,
    args: PathBuf,
}

/// A project over a real repo, whose `claude` prints `lines` (or, with `None`, exits 3 at once).
fn rig(lines: Option<&str>, choice: EngineChoiceConfig, claude: bool) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let repo = crate::test_support::repo_with_origin(&root);
    let (out, args, bin) = (root.join("out.jsonl"), root.join("args.txt"), root.join(fake_name()));
    std::fs::write(&out, lines.unwrap_or("")).unwrap();
    if claude {
        fake_claude(&bin, &out, &args, lines.is_some());
    }
    let project = project_in(&root, &repo);
    let bin = bin.to_string_lossy().into_owned();
    let gh = root.join(if cfg!(windows) { "gh.cmd" } else { "gh.sh" });
    fake_gh(&gh, &root);
    let gh = gh.to_string_lossy().into_owned();
    let env = Env::new(move |k| match k {
        "RETASKABLE_CLAUDE" => Some(bin.clone()),
        "RETASKABLE_GH" => Some(gh.clone()),
        _ => None,
    });
    let starter = ClaudeStarter::new(
        root.join("data"),
        vec![project],
        choice,
        env,
        50,
        ModelsConfig::default(),
        150_000,
    );
    Rig {
        _dir: dir,
        root,
        starter,
        args,
    }
}

/// A `gh` that logs its arguments to `gh.log` in `root`, lists the JSON in `gh_list` for `pr list`, and for
/// `pr create` saves its stdin as `gh_body` and prints a PR URL.
pub(in crate::orchestrator) fn fake_gh(bin: &Path, root: &Path) {
    std::fs::write(root.join("gh_list"), "[]").unwrap();
    std::fs::write(root.join("gh_state"), "OPEN").unwrap();
    let r = root.display();
    if cfg!(windows) {
        let script = format!(
            "@echo off\r\necho %* >> \"{r}\\gh.log\"\r\nif \"%1 %2\"==\"pr view\" type \"{r}\\gh_state\"\r\nif \"%1 %2\"==\"pr list\" type \"{r}\\gh_list\"\r\nif \"%1 %2\"==\"pr create\" (findstr \"^\" > \"{r}\\gh_body\" & echo https://github.com/o/p/pull/7)\r\n"
        );
        std::fs::write(bin, script).unwrap();
    } else {
        let script = format!(
            "#!/bin/sh\necho \"$@\" >> '{r}/gh.log'\ncase \"$1 $2\" in\n\"pr view\") cat '{r}/gh_state';;\n\"pr list\") cat '{r}/gh_list';;\n\"pr create\") cat > '{r}/gh_body'; echo https://github.com/o/p/pull/7;;\n\"pr comment\") cat >> '{r}/gh_comment'; echo '---' >> '{r}/gh_comment';;\nesac\n"
        );
        crate::test_support::write_executable(bin, &script);
    }
}

/// The branches on the rig's bare origin.
fn origin_branches(rig: &Rig) -> String {
    let out = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(rig.root.join("origin.git"))
        .args(["branch", "--list"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// What the fake `gh` was asked, without the bare run that `write_executable` makes to check the script starts.
fn gh_log(rig: &Rig) -> String {
    let log = std::fs::read_to_string(rig.root.join("gh.log")).unwrap_or_default();
    log.lines()
        .filter(|l| l.starts_with("pr "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn branch() -> String {
    branch_name(&item(&[], Some("Wire it.")), 0)
}

fn item(spec_refs: &[&str], remaining: Option<&str>) -> Item {
    Item {
        id: "implementer-session".into(),
        title: "Have a session write the code".into(),
        status: ItemStatus::Building,
        rank: 1,
        do_next: false,
        depends_on: Vec::new(),
        spec_refs: spec_refs.iter().map(ToString::to_string).collect(),
        slice: 2,
        remaining: remaining.map(str::to_string),
        pr_url: None,
        branch: None,
        kind: None,
        milestone: None,
        reopened: None,
    }
}

pub(in crate::orchestrator) fn fake_name() -> &'static str {
    if cfg!(windows) { "claude.cmd" } else { "claude.sh" }
}

fn fake_claude(bin: &Path, out: &Path, args: &Path, ok: bool) {
    let code = if ok { 0 } else { 3 };
    if cfg!(windows) {
        let script = format!(
            "@echo off\r\necho %* > \"{0}\"\r\nfindstr \"^\" >> \"{0}\"\r\ntype \"{1}\"\r\nexit /b {code}\r\n",
            args.display(),
            out.display()
        );
        std::fs::write(bin, script).unwrap();
    } else {
        let script = format!(
            "#!/bin/sh\necho \"$@\" > \"{0}\"\ncat >> \"{0}\"\ncat \"{1}\"\nexit {code}\n",
            args.display(),
            out.display()
        );
        crate::test_support::write_executable(bin, &script);
    }
}

fn queued_item() -> Item {
    Item {
        id: "queued-one".into(),
        title: "Another queued item".into(),
        status: ItemStatus::Todo,
        ..item(&[], None)
    }
}

fn request() -> (StartRequest, UnboundedReceiver<SessionOutcome>) {
    let (outcomes, rx) = OutcomeSender::channel();
    let request = StartRequest {
        project: "p".into(),
        item_id: "implementer-session".into(),
        lane: 1,
        token: "t".into(),
        item: item(&[], Some("Wire it.")),
        session_id: "sess-1".into(),
        engine: EngineId::Claude,
        resume: false,
        outcomes,
        usage: None,
        follow_up: None,
        items: vec![item(&[], None), queued_item()],
        attempt: 0,
        notes: None,
        takeover: None,
    };
    (request, rx)
}

/// Starts one session and waits for its outcome, checking that no second one follows.
async fn outcome_of(rig: &Rig) -> SessionResult {
    let (request, mut rx) = request();
    rig.starter.start(request).unwrap();
    let got = tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .expect("an outcome")
        .expect("the channel stays open");
    assert_eq!((got.project.as_str(), got.lane), ("p", 1));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(rx.try_recv().is_err(), "exactly one outcome per start");
    got.result
}

fn answer(status: &str, questions: &str) -> String {
    format!(
        r#"{{"type":"result","subtype":"success","is_error":false,"structured_output":{{"status":"{status}","summary":"s","questions":{questions}}},"session_id":"s"}}"#
    )
}

#[tokio::test]
async fn a_finished_session_sends_finished_and_keeps_its_transcript_locally() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    assert_eq!(
        outcome_of(&rig).await,
        SessionResult::Implementer(ImplementerOutcome::Finished)
    );
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(args.contains("--permission-mode auto"), "{args}");
    assert!(!args.contains("--dangerously-skip-permissions"), "{args}");
    let runs = paths::run_dir(&rig.root.join("data"), "p", "implementer-session");
    let files: Vec<_> = std::fs::read_dir(runs).unwrap().collect();
    assert_eq!(files.len(), 1);
}

#[cfg(unix)]
const FINISHED_WITH_TITLE: &str = r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened","summary":"done","changeTitle":"item: My title","changeBody":"My body"},"session_id":"s"}"#;

#[cfg(unix)]
#[tokio::test]
async fn a_finished_session_is_pushed_and_its_pr_opened_with_its_title_body_and_base() {
    let rig = gated_rig(&[COMMIT_SMALL]);
    std::fs::write(rig.root.join("out.jsonl"), FINISHED_WITH_TITLE).unwrap();
    let got = gated_outcome(&rig).await;
    assert_eq!(got.result, SessionResult::Implementer(ImplementerOutcome::Finished));
    assert!(
        origin_branches(&rig).contains("retaskable/implementer-session"),
        "{}",
        origin_branches(&rig)
    );
    let log = gh_log(&rig);
    assert!(
        log.contains(&format!("pr list --head {} --state open", branch())),
        "{log}"
    );
    assert!(
        log.contains(&format!(
            "pr create --base main --head {} --title item: My title",
            branch()
        )),
        "{log}"
    );
    assert_eq!(std::fs::read_to_string(rig.root.join("gh_body")).unwrap(), "My body");
    assert_eq!(got.notes, vec!["Opened PR #7: https://github.com/o/p/pull/7"]);
}

#[cfg(unix)]
#[tokio::test]
async fn an_open_pr_for_the_branch_means_only_the_push_happens() {
    let rig = gated_rig(&[COMMIT_SMALL]);
    std::fs::write(
        rig.root.join("gh_list"),
        r#"[{"number":4,"url":"https://github.com/o/p/pull/4"}]"#,
    )
    .unwrap();
    let got = gated_outcome(&rig).await;
    assert_eq!(got.result, SessionResult::Implementer(ImplementerOutcome::Finished));
    assert!(origin_branches(&rig).contains(&branch()));
    assert!(!gh_log(&rig).contains("pr create"), "{}", gh_log(&rig));
    assert_eq!(
        got.notes,
        vec!["Pushed to the open PR #4: https://github.com/o/p/pull/4"]
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_push_failure_needs_a_person_and_opens_nothing() {
    let rig = gated_rig(&[COMMIT_SMALL]);
    let url = rig.root.join("nowhere.git");
    let set = std::process::Command::new("git")
        .current_dir(rig.root.join("repo"))
        .args(["remote", "set-url", "--push", "origin"])
        .arg(&url)
        .status()
        .unwrap();
    assert!(set.success());
    let got = gated_outcome(&rig).await;
    match got.result {
        SessionResult::NeedsHuman(why) => assert!(why.contains("couldn't be published"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(gh_log(&rig), "");
}

/// A `claude` that, on its `n`th run in the lane, does `steps[n-1]` (shell, run in the lane), saves the
/// prompt it was given as `prompt<n>.txt` in the rig's folder, and answers `pr_opened`.
#[cfg(unix)]
fn gated_rig(steps: &[&str]) -> Rig {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    let mut script = format!(
        "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nR='{}'\nn=$(cat \"$R/count\" 2>/dev/null || echo 0); n=$((n+1)); echo $n > \"$R/count\"\necho \"$@\" >> \"$R/args_all\"\ncat > \"$R/prompt$n.txt\"\ncase $n in\n",
        rig.root.display()
    );
    for (i, step) in steps.iter().enumerate() {
        script += &format!("{}) {step};;\n", i + 1);
    }
    script += &format!("esac\ncat \"{}\"\n", rig.root.join("out.jsonl").display());
    crate::test_support::write_executable(&rig.root.join(fake_name()), &script);
    rig
}

#[cfg(unix)]
const COMMIT_BIG: &str = "seq 1 1100 | sed 's/^/let v/;s/$/ = 1;/' > big.rs; git add -A; git -c user.name=t -c user.email=t@t commit -qm big";
#[cfg(unix)]
const COMMIT_SMALL: &str =
    "echo 'fn a() {}' > small.rs; git add -A; git -c user.name=t -c user.email=t@t commit -qm small";
#[cfg(unix)]
const FIX: &str = "git rm -q big.rs; git -c user.name=t -c user.email=t@t commit -qm fix";

#[cfg(unix)]
async fn gated_outcome(rig: &Rig) -> SessionOutcome {
    let (request, mut rx) = request();
    rig.starter.start(request).unwrap();
    tokio::time::timeout(Duration::from_secs(120), rx.recv())
        .await
        .expect("an outcome")
        .expect("the channel stays open")
}

#[cfg(unix)]
fn runs_of(rig: &Rig) -> usize {
    std::fs::read_to_string(rig.root.join("count"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[cfg(unix)]
#[tokio::test]
async fn a_change_that_passes_the_gates_finishes_after_one_run() {
    let rig = gated_rig(&[COMMIT_SMALL]);
    let got = gated_outcome(&rig).await;
    assert_eq!(got.result, SessionResult::Implementer(ImplementerOutcome::Finished));
    assert_eq!(got.notes, vec!["Opened PR #7: https://github.com/o/p/pull/7"]);
    assert_eq!(runs_of(&rig), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn a_file_over_the_limit_resumes_the_session_with_the_finding_until_the_change_passes() {
    let rig = gated_rig(&[COMMIT_BIG, FIX]);
    let got = gated_outcome(&rig).await;
    assert_eq!(got.result, SessionResult::Implementer(ImplementerOutcome::Finished));
    assert_eq!(runs_of(&rig), 2);
    let prompt = std::fs::read_to_string(rig.root.join("prompt2.txt")).unwrap();
    assert!(prompt.contains("big.rs"), "{prompt}");
    assert!(
        prompt.contains("Fix them, commit, and return the result again"),
        "{prompt}"
    );
    let args = std::fs::read_to_string(rig.root.join("args_all")).unwrap();
    assert!(args.lines().nth(1).unwrap().contains("--resume sess-1"), "{args}");
    assert_eq!(got.notes.len(), 2);
    assert!(
        got.notes[0].starts_with("Code gates: 1 file over 1000 lines"),
        "{:?}",
        got.notes
    );
    let runs = paths::run_dir(&rig.root.join("data"), "p", "implementer-session");
    assert_eq!(
        std::fs::read_dir(runs).unwrap().count(),
        2,
        "a new transcript per round"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn findings_past_max_check_rounds_need_a_person() {
    // Every run adds another oversized file, so the gates never pass.
    let step = |n: u32| COMMIT_BIG.replace("big.rs", &format!("big{n}.rs"));
    let steps: Vec<String> = (1..=4).map(step).collect();
    let rig = gated_rig(&steps.iter().map(String::as_str).collect::<Vec<_>>());
    let got = gated_outcome(&rig).await;
    match got.result {
        SessionResult::NeedsHuman(why) => assert!(why.contains("big1.rs"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(runs_of(&rig), 4, "the first run and three rounds of fixes");
    assert!(!origin_branches(&rig).contains("retaskable/"), "nothing is pushed");
    assert_eq!(gh_log(&rig), "");
    assert_eq!(got.notes.len(), 4);
}

#[cfg(unix)]
#[tokio::test]
async fn gates_that_cannot_run_are_noted_and_the_session_still_finishes() {
    let rig = gated_rig(&["git update-ref -d refs/remotes/origin/main"]);
    let got = gated_outcome(&rig).await;
    assert_eq!(got.result, SessionResult::Implementer(ImplementerOutcome::Finished));
    assert_eq!(runs_of(&rig), 1);
    assert_eq!(got.notes.len(), 2);
    assert!(got.notes[0].starts_with("Code gates couldn't run"), "{:?}", got.notes);
}

#[tokio::test]
async fn a_fresh_prompt_lists_the_unbuilt_items_and_a_resumed_one_does_not() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    outcome_of(&rig).await;
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(args.contains("Items not built yet"), "{args}");
    assert!(args.contains("- queued-one: Another queued item"), "{args}");
    assert!(!args.contains("- implementer-session: Have"), "{args}");
    let (mut request, mut rx) = request();
    request.resume = true;
    rig.starter.start(request).unwrap();
    tokio::time::timeout(Duration::from_secs(120), rx.recv()).await.unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(
        !args.contains("Items not built yet") && !args.contains("queued-one"),
        "{args}"
    );
}

#[tokio::test]
async fn a_fresh_session_runs_under_the_given_id_and_a_resumed_one_continues_it() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    outcome_of(&rig).await;
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(
        args.contains("--session-id sess-1") && !args.contains("--resume"),
        "{args}"
    );
    let (mut request, mut rx) = request();
    request.resume = true;
    rig.starter.start(request).unwrap();
    let got = tokio::time::timeout(Duration::from_secs(120), rx.recv()).await;
    assert!(matches!(got, Ok(Some(_))));
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(
        args.contains("--resume sess-1") && !args.contains("--session-id"),
        "{args}"
    );
    let runs = paths::run_dir(&rig.root.join("data"), "p", "implementer-session");
    assert_eq!(
        std::fs::read_dir(runs).unwrap().count(),
        2,
        "the first transcript is kept"
    );
}

/// Commits a spec that names `src/lib.rs`, and that file, to the rig's repo and origin.
fn commit_spec(rig: &Rig) {
    let repo = rig.root.join("repo");
    std::fs::create_dir_all(repo.join("docs/specs")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("docs/specs/a.md"), "Touches `src/lib.rs`.\n").unwrap();
    std::fs::write(repo.join("src/lib.rs"), "").unwrap();
    for args in [
        vec!["add", "."],
        vec!["commit", "--quiet", "-m", "spec"],
        vec!["push", "--quiet", "origin", "main"],
    ] {
        let ok = crate::test_support::git(&repo).args(args).status().unwrap().success();
        assert!(ok);
    }
}

async fn start_with(rig: &Rig, spec_refs: &[&str], resume: bool) {
    let (mut request, mut rx) = request();
    request.item.spec_refs = spec_refs.iter().map(ToString::to_string).collect();
    request.resume = resume;
    rig.starter.start(request).unwrap();
    let got = tokio::time::timeout(Duration::from_secs(120), rx.recv()).await;
    assert!(matches!(got, Ok(Some(_))));
}

#[tokio::test]
async fn a_fresh_session_is_pointed_at_the_items_specs_and_the_code_they_name() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    commit_spec(&rig);
    start_with(&rig, &["docs/specs/a.md"], false).await;
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(args.contains("## Where to start"), "{args}");
    assert!(
        args.contains("docs/specs/a.md") && args.contains("src/lib.rs"),
        "{args}"
    );
}

#[tokio::test]
async fn a_resumed_session_gets_only_the_follow_up() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    commit_spec(&rig);
    start_with(&rig, &["docs/specs/a.md"], false).await;
    start_with(&rig, &["docs/specs/a.md"], true).await;
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(args.contains("--resume sess-1"), "{args}");
    assert!(
        !args.contains("## Where to start") && !args.contains("docs/specs/a.md"),
        "{args}"
    );
}

#[tokio::test]
async fn a_session_still_starts_when_git_gives_nothing_to_list() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    // Break the lane's view of origin after it's prepared is not possible, so name specs git can't show.
    start_with(&rig, &["docs/specs/missing.md"], false).await;
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(args.contains("--session-id sess-1"), "{args}");
    assert!(!args.contains("## Where to start"), "{args}");
}

#[tokio::test]
async fn a_session_that_exits_without_an_answer_needs_a_human() {
    let rig = rig(None, EngineChoiceConfig::default(), true);
    assert!(matches!(outcome_of(&rig).await, SessionResult::NeedsHuman(_)));
}

fn classified(json: &str) -> SessionResult {
    classify(&serde_json::from_str(json).unwrap())
}

#[test]
fn each_status_maps_to_what_the_ts_agent_does() {
    let why = |r: SessionResult| match r {
        SessionResult::NeedsHuman(why) => why,
        other => panic!("{other:?}"),
    };
    assert!(why(classified(r#"{"status":"failed","summary":"stuck"}"#)).contains("Implementer gave up: stuck"));
    assert!(why(classified(r#"{"status":"nothing_left","summary":"built"}"#)).contains("Nothing left to build"));
    let steps = why(classified(
        r#"{"status":"person_steps","personSteps":["Press go"],"summary":"s"}"#,
    ));
    assert!(steps.contains("1. Press go"), "{steps}");
    assert!(why(classified(r#"{"status":"person_steps","summary":"s"}"#)).contains("gave up"));
    let waits = why(classified(r#"{"status":"waits_on","waitsOn":["a","b"],"summary":"s"}"#));
    assert!(waits.contains("a, b"), "{waits}");
    assert!(why(classified(r#"{"status":"odd","summary":"s"}"#)).contains("answered with odd"));
}

#[test]
fn a_watchdog_kill_is_an_own_failure_not_a_needs_human() {
    let killed = Err(anyhow::Error::new(SilenceKilled(
        "ended with killed_for_silence: no output".into(),
    )));
    assert!(
        matches!(session_result_for_answer(killed, None).0, SessionResult::Failed(_)),
        "a session the watchdog killed should requeue, not need a person"
    );
    let crashed = Err(anyhow::anyhow!("ended with error_during_execution"));
    assert!(matches!(
        session_result_for_answer(crashed, None).0,
        SessionResult::NeedsHuman(_)
    ));
}

#[test]
fn a_cut_sessions_error_maps_to_interrupted_not_needs_human_or_failed() {
    let cut = Err(anyhow::Error::new(Interrupted("ended with interrupted".into())));
    assert_eq!(session_result_for_answer(cut, None).0, SessionResult::Interrupted);

    let killed = Err(anyhow::Error::new(SilenceKilled(
        "ended with killed_for_silence: no output".into(),
    )));
    assert!(matches!(
        session_result_for_answer(killed, None).0,
        SessionResult::Failed(_)
    ));

    let crashed = Err(anyhow::anyhow!("ended with error_during_execution"));
    assert!(matches!(
        session_result_for_answer(crashed, None).0,
        SessionResult::NeedsHuman(_)
    ));
}

#[test]
fn the_usage_reading_travels_with_the_result_whichever_way_the_session_ended() {
    let usage = serde_json::json!({"status": "rejected", "resetsAt": i64::MAX});
    let killed = Err(anyhow::Error::new(SilenceKilled(
        "ended with killed_for_silence".into(),
    )));
    assert_eq!(
        session_result_for_answer(killed, Some(usage.clone())).1,
        Some(usage.clone())
    );
    let crashed = Err(anyhow::anyhow!("ended with error_during_execution"));
    assert_eq!(session_result_for_answer(crashed, Some(usage.clone())).1, Some(usage));
}

#[test]
fn the_sandbox_follows_the_platform() {
    use crate::config::SandboxMode;
    assert!(sandbox_enabled(SandboxMode::Auto, false));
    assert!(!sandbox_enabled(SandboxMode::Auto, true));
    assert!(!sandbox_enabled(SandboxMode::Off, false));
}

#[tokio::test]
async fn a_sandboxed_platform_passes_the_sandbox_settings() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    outcome_of(&rig).await;
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert_eq!(args.contains("--settings"), !cfg!(windows), "{args}");
}

#[tokio::test]
async fn a_session_with_questions_sends_questions() {
    let rig = rig(
        Some(&answer("needs_clarification", r#"[{"question":"Which one?"}]"#)),
        EngineChoiceConfig::default(),
        true,
    );
    assert_eq!(
        outcome_of(&rig).await,
        SessionResult::Implementer(ImplementerOutcome::Questions {
            all_already_answered: false
        })
    );
}

#[tokio::test]
async fn no_usage_reading_yet_still_starts_a_first_try_on_sonnet() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    let (mut request, mut rx) = request();
    request.usage = None;
    rig.starter.start(request).unwrap();
    rx.recv().await.unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    let model = args.split_whitespace().skip_while(|a| *a != "--model").nth(1);
    assert_eq!(model, Some("sonnet"), "{args}");
}

#[tokio::test]
async fn a_reopened_item_with_no_usage_reading_starts_on_opus() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    let (mut request, mut rx) = request();
    request.usage = None;
    request.item.reopened = Some(crate::backlog::Reopened {
        notes: Some("The list is empty".into()),
        at: Some("2026-10-29T00:00:00Z".into()),
        by: None,
    });
    rig.starter.start(request).unwrap();
    rx.recv().await.unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    let model = args.split_whitespace().skip_while(|a| *a != "--model").nth(1);
    assert_eq!(model, Some("opus"), "{args}");
}

#[tokio::test]
async fn a_usage_reading_below_opus_above_pct_starts_the_implementer_on_sonnet() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    let (mut request, mut rx) = request();
    request.usage = Some(serde_json::json!({"status": "allowed_warning", "resetsAt": i64::MAX}));
    rig.starter.start(request).unwrap();
    rx.recv().await.unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    let model = args.split_whitespace().skip_while(|a| *a != "--model").nth(1);
    assert_eq!(model, Some("sonnet"), "{args}");
}

/// A starter like `rig`'s, but whose only project's repo path is missing, so preparing its lane fails.
fn starter_with_unpreparable_lane(rig: &Rig) -> Arc<ClaudeStarter> {
    let mut projects = rig.starter.projects.clone();
    projects[0].repo_path = rig.root.join("missing").to_string_lossy().into_owned();
    ClaudeStarter::new(
        rig.root.join("data"),
        projects,
        EngineChoiceConfig::default(),
        rig.starter.env.clone(),
        50,
        ModelsConfig::default(),
        150_000,
    )
}

#[tokio::test]
async fn a_lane_that_cannot_be_prepared_fails() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    let starter = starter_with_unpreparable_lane(&rig);
    let (request, mut rx) = request();
    starter.start(request).unwrap();
    let got = rx.recv().await.unwrap();
    assert!(matches!(got.result, SessionResult::Failed(_)), "{got:?}");
}

#[tokio::test]
async fn a_resumed_session_that_cannot_prepare_its_lane_needs_a_human_not_the_queue() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    let starter = starter_with_unpreparable_lane(&rig);
    let (mut request, mut rx) = request();
    request.resume = true;
    starter.start(request).unwrap();
    let got = rx.recv().await.unwrap();
    assert!(matches!(got.result, SessionResult::NeedsHuman(_)), "{got:?}");
}

#[tokio::test]
async fn another_engine_refuses_to_start() {
    let choice = EngineChoiceConfig {
        roles: RoleEngines {
            implementer: Some(Engine::Cursor),
            ..Default::default()
        },
        ..Default::default()
    };
    let rig = rig(Some(FINISHED), choice, true);
    let (request, mut rx) = request();
    assert_eq!(
        rig.starter.start(request),
        Err("only Claude sessions run in the Rust agent yet".to_string())
    );
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn a_missing_binary_refuses_to_start() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), false);
    let (request, mut rx) = request();
    let err = rig.starter.start(request).unwrap_err();
    assert!(err.contains("wasn't found"), "{err}");
    assert!(rx.try_recv().is_err());
}

#[test]
fn the_branch_is_the_id_a_short_slug_and_the_slice() {
    assert_eq!(
        branch_name(&item(&[], None), 0),
        "retaskable/implementer-session-have-a-session-write-the-code-s2"
    );
}

#[test]
fn quiet_timeout_turns_off_at_zero_and_is_minutes_otherwise() {
    assert_eq!(quiet_timeout(0), None);
    assert_eq!(quiet_timeout(20), Some(Duration::from_secs(20 * 60)));
}

#[cfg(unix)]
const OUT_OF_TURNS: &str =
    r#"printf '%s\n' '{"type":"result","subtype":"error_max_turns","is_error":false,"session_id":"s"}'; exit 0"#;

/// The fake's first run leaves an uncommitted file, then runs out of turns.
#[cfg(unix)]
const SPEND_TURNS: &str = "echo wip > wip.txt";

#[cfg(unix)]
fn session_of(rig: &Rig, n: usize) -> String {
    let args = std::fs::read_to_string(rig.root.join("args_all")).unwrap();
    args.lines().nth(n).unwrap().to_string()
}

#[cfg(unix)]
#[tokio::test]
async fn an_attempt_that_runs_out_of_turns_is_followed_by_a_fresh_session_in_the_same_lane() {
    let first = format!("{SPEND_TURNS}; {OUT_OF_TURNS}");
    let rig = gated_rig(&[&first, "test -f wip.txt && echo present > \"$R/wip_seen\""]);
    let got = gated_outcome(&rig).await;
    assert_eq!(got.result, SessionResult::Implementer(ImplementerOutcome::Finished));
    assert_eq!(runs_of(&rig), 2);
    let args = session_of(&rig, 1);
    assert!(args.contains("--session-id"), "{args}");
    assert!(!args.contains("--resume"), "{args}");
    let new_id = got.session_id.expect("the new session id comes back");
    assert!(args.contains(&new_id), "{args}");
    assert_ne!(new_id, "sess-1");
    let prompt = std::fs::read_to_string(rig.root.join("prompt2.txt")).unwrap();
    assert!(prompt.starts_with("You're picking up"), "{prompt}");
    assert!(
        prompt.ends_with(crate::orchestrator::fresh_start::MAX_TURNS_FOLLOW_UP),
        "{prompt}"
    );
    assert!(
        rig.root.join("wip_seen").exists(),
        "the uncommitted file is still there"
    );
    assert!(
        got.notes
            .contains(&"An earlier session ran out of turns; starting a fresh one".to_string()),
        "{:?}",
        got.notes
    );
}

#[cfg(unix)]
const BIG_SESSION: &str = r#"{"type":"assistant","parent_tool_use_id":null,"message":{"usage":{"input_tokens":1000,"cache_read_input_tokens":%N%,"cache_creation_input_tokens":0}}}
{"type":"result","subtype":"success","is_error":false,"structured_output":{"status":"pr_opened","summary":"done"},"session_id":"s"}"#;

#[cfg(unix)]
#[tokio::test]
async fn a_send_back_to_a_grown_session_starts_a_fresh_one_with_the_findings() {
    let rig = gated_rig(&[COMMIT_BIG, FIX]);
    std::fs::write(rig.root.join("out.jsonl"), BIG_SESSION.replace("%N%", "200000")).unwrap();
    let got = gated_outcome(&rig).await;
    assert_eq!(got.result, SessionResult::Implementer(ImplementerOutcome::Finished));
    let new = got.session_id.expect("the new session's id");
    assert_ne!(new, "sess-1");
    let args = std::fs::read_to_string(rig.root.join("args_all")).unwrap();
    let second = args.lines().nth(1).unwrap();
    assert!(second.contains(&format!("--session-id {new}")), "{second}");
    assert!(!second.contains("--resume"), "{second}");
    let prompt = std::fs::read_to_string(rig.root.join("prompt2.txt")).unwrap();
    assert!(prompt.starts_with("You're picking up"), "{prompt}");
    assert!(prompt.contains("big.rs"), "{prompt}");
    assert!(
        got.notes
            .iter()
            .any(|n| n == "Its session had grown to 201000 tokens; starting a fresh one"),
        "{:?}",
        got.notes
    );
}

#[cfg(unix)]
#[tokio::test]
async fn two_attempts_in_a_row_that_run_out_of_turns_need_a_person() {
    let rig = gated_rig(&[OUT_OF_TURNS, OUT_OF_TURNS]);
    let got = gated_outcome(&rig).await;
    assert_eq!(
        got.result,
        SessionResult::NeedsHuman("Implementer ran out of turns (50) twice in a row".into())
    );
    assert_eq!(runs_of(&rig), 2);
    assert!(got.session_id.is_some());
}

#[cfg(unix)]
#[tokio::test]
async fn a_send_back_that_runs_out_of_turns_also_restarts_fresh() {
    let rig = gated_rig(&[COMMIT_BIG, OUT_OF_TURNS, FIX]);
    let got = gated_outcome(&rig).await;
    assert_eq!(got.result, SessionResult::Implementer(ImplementerOutcome::Finished));
    assert_eq!(runs_of(&rig), 3);
    assert!(session_of(&rig, 1).contains("--resume sess-1"));
    let fresh = session_of(&rig, 2);
    assert!(fresh.contains("--session-id"), "{fresh}");
    let prompt = std::fs::read_to_string(rig.root.join("prompt3.txt")).unwrap();
    assert!(
        prompt.ends_with(crate::orchestrator::fresh_start::MAX_TURNS_FOLLOW_UP),
        "{prompt}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_send_back_to_a_session_under_the_limit_still_resumes_it() {
    let rig = gated_rig(&[COMMIT_BIG, FIX]);
    std::fs::write(rig.root.join("out.jsonl"), BIG_SESSION.replace("%N%", "1000")).unwrap();
    let got = gated_outcome(&rig).await;
    assert_eq!(got.session_id, None);
    let args = std::fs::read_to_string(rig.root.join("args_all")).unwrap();
    assert!(args.lines().nth(1).unwrap().contains("--resume sess-1"), "{args}");
}

fn git_in(rig: &Rig, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(rig.root.join("repo"))
        .args(args)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn old_work(rig: &Rig, pr_url: Option<&str>) -> Abandon {
    git_in(rig, &["branch", "old"]);
    git_in(rig, &["push", "origin", "old"]);
    Abandon {
        project: "p".into(),
        branch: Some("old".into()),
        pr_url: pr_url.map(Into::into),
    }
}

#[tokio::test]
async fn start_over_closes_the_old_pr_and_leaves_its_branch() {
    let rig = rig(None, EngineChoiceConfig::default(), false);
    rig.starter
        .abandon(old_work(&rig, Some("https://github.com/o/p/pull/3")))
        .await
        .unwrap();
    assert!(
        gh_log(&rig).contains("pr close https://github.com/o/p/pull/3"),
        "{}",
        gh_log(&rig)
    );
    assert!(origin_branches(&rig).contains("old"));
}

#[tokio::test]
async fn start_over_leaves_a_merged_slice_alone() {
    let rig = rig(None, EngineChoiceConfig::default(), false);
    std::fs::write(rig.root.join("gh_state"), "MERGED").unwrap();
    rig.starter
        .abandon(old_work(&rig, Some("https://github.com/o/p/pull/3")))
        .await
        .unwrap();
    assert!(!gh_log(&rig).contains("pr close"), "{}", gh_log(&rig));
    assert!(origin_branches(&rig).contains("old"));
    assert!(git_in(&rig, &["branch", "--list", "old"]).contains("old"));
}

#[tokio::test]
async fn start_over_without_a_pr_drops_the_old_branch_pushed_or_not() {
    let rig = rig(None, EngineChoiceConfig::default(), false);
    rig.starter.abandon(old_work(&rig, None)).await.unwrap();
    assert!(!gh_log(&rig).contains("pr close"));
    assert!(!origin_branches(&rig).contains("old"), "{}", origin_branches(&rig));
    assert!(!git_in(&rig, &["branch", "--list", "old"]).contains("old"));
    // Never pushed: only here.
    git_in(&rig, &["branch", "local-only"]);
    let old = Abandon {
        branch: Some("local-only".into()),
        ..old_work(&rig, None)
    };
    rig.starter.abandon(old).await.unwrap();
    assert!(!git_in(&rig, &["branch", "--list", "local-only"]).contains("local-only"));
}

#[tokio::test]
async fn a_start_over_session_is_on_the_attempts_branch_with_the_notes_in_its_first_prompt() {
    let rig = rig(Some(FINISHED), EngineChoiceConfig::default(), true);
    let (mut request, mut rx) = request();
    request.attempt = 2;
    request.notes = Some("Use the other API.".into());
    rig.starter.start(request).unwrap();
    tokio::time::timeout(Duration::from_secs(120), rx.recv()).await.unwrap();
    let args = std::fs::read_to_string(&rig.args).unwrap();
    assert!(
        args.contains("retaskable/implementer-session-have-a-session-write-the-code-s2-r2"),
        "{args}"
    );
    assert!(args.contains("## Notes from the last attempt"), "{args}");
    assert!(args.contains("Use the other API."), "{args}");
    assert_eq!(branch_name(&item(&[], None), 0), branch());
    assert!(branch().ends_with("-s2"));
}

#[test]
fn a_piece_branch_is_its_id_and_the_title_slug() {
    assert_eq!(
        piece_branch_name("p7", "Build the implementer's prompt!"),
        "retaskable/p7-build-the-implementer-s-prompt"
    );
}

/// The rig project "p" over `repo`, with its lanes under `root`.
pub(in crate::orchestrator) fn project_in(root: &Path, repo: &Path) -> ProjectConfig {
    let mut project = ProjectConfig {
        id: Some("p".into()),
        repo_path: repo.to_string_lossy().into_owned(),
        worktrees_dir: root.join("worktrees").to_string_lossy().into_owned(),
        base_branch: "main".into(),
        github_repo: "o/p".into(),
        max_check_rounds: 3,
        ..Default::default()
    };
    project.budget.implementer_usd = 5.0;
    project
}
