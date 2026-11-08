//! What the read-only run starters' tests (`plan_run_tests.rs`, `look_run_tests.rs`) share: a fake `claude` that
//! records how it ran, and the checks on that record.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::UnboundedReceiver;

use crate::config::ProjectConfig;
use crate::env::Env;
use crate::orchestrator::session::tests::{fake_name, project_in};
use crate::process_tree::ProcessTree;

pub(super) struct Fake {
    pub dir: tempfile::TempDir,
    pub root: PathBuf,
    pub env: Env,
    pub project: ProjectConfig,
}

/// A project whose `claude` records its arguments, prompt (`in.txt`) and folder, then prints `out`; `extra` runs first.
pub(super) fn fake_claude(out: &str, extra: &str) -> Fake {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let script = format!(
        "#!/bin/sh\n[ $# -eq 0 ] && exit 0\nR='{r}'\ncat > \"$R/in.txt\"\necho \"$@\" > \"$R/args.txt\"\npwd > \"$R/pwd.txt\"\n{extra}\ncat \"$R/out.jsonl\"\n",
        r = root.display()
    );
    let bin = root.join(fake_name());
    crate::test_support::write_executable(&bin, &script);
    std::fs::write(root.join("out.jsonl"), out).unwrap();
    let bin = bin.to_string_lossy().into_owned();
    let env = Env::new(move |k| (k == "RETASKABLE_CLAUDE").then(|| bin.clone()));
    let project = project_in(&root, &repo);
    Fake {
        dir,
        root,
        env,
        project,
    }
}

/// The fake `claude` ran in `dir`, read-only, on Sonnet, with a $1.5 budget.
pub(super) fn assert_ran_read_only_in(root: &Path, dir: &Path) {
    let pwd = std::fs::read_to_string(root.join("pwd.txt")).unwrap();
    assert_eq!(
        std::fs::canonicalize(pwd.trim()).unwrap(),
        std::fs::canonicalize(dir).unwrap()
    );
    let args = std::fs::read_to_string(root.join("args.txt")).unwrap();
    assert!(
        args.contains("--max-budget-usd 1.5") && args.contains("sonnet"),
        "{args}"
    );
    assert!(
        !args.contains("--dangerously") && args.contains("--disallowedTools") && args.contains("Edit Write"),
        "{args}"
    );
}

/// Takes the session's registered tree and cuts it, as a stop would.
pub(super) async fn cut_registered_tree(trees: &mut UnboundedReceiver<(String, Arc<ProcessTree>)>) {
    let (token, tree) = tokio::time::timeout(Duration::from_secs(30), trees.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(token, "tok");
    tree.cut();
    tree.kill_and_wait(Duration::from_secs(5));
}

/// A fake `claude` project as a plan/look roster, `tweak`ing its config first.
pub(super) fn roster(
    out: &str,
    extra: &str,
    choice: crate::config::EngineChoiceConfig,
    tweak: impl FnOnce(&mut ProjectConfig),
) -> (tempfile::TempDir, PathBuf, crate::orchestrator::plan_run::Roster) {
    let Fake {
        dir,
        root,
        env,
        mut project,
    } = fake_claude(out, extra);
    tweak(&mut project);
    let roster = crate::orchestrator::plan_run::Roster {
        data_dir: root.join("data"),
        projects: vec![project],
        choice,
        env,
        models: Default::default(),
        cursor_models: Default::default(),
        flags: Arc::new(crate::flags::Flags::fixed(&["engine-placement"])),
    };
    (dir, root, roster)
}

/// Points every engine's binary variable at the fake `claude` stub, and Cursor's `top` model at `gpt-top`, so a
/// session on any engine runs the stub; then the stub's `args.txt` says which engine ran.
pub(super) fn on_every_engine(roster: &mut crate::orchestrator::plan_run::Roster, root: &Path) {
    let bin = root.join(fake_name()).to_string_lossy().into_owned();
    roster.env = Env::new(move |k| {
        [
            "RETASKABLE_CLAUDE",
            "RETASKABLE_CURSOR",
            "RETASKABLE_PI",
            "RETASKABLE_CODEX",
        ]
        .contains(&k)
        .then(|| bin.clone())
    });
    roster.cursor_models.top = Some("gpt-top".into());
}

/// The arguments the fake binary was last run with, for the engine `program` (`cursor`, `codex` or `pi`).
pub(super) fn assert_ran_on(root: &Path, program: &str) {
    let args = std::fs::read_to_string(root.join("args.txt")).unwrap();
    match program {
        "cursor" => assert!(args.contains("gpt-top"), "{args}"),
        "codex" => assert!(args.contains("exec"), "{args}"),
        _ => assert!(
            !args.contains("--permission-mode") && !args.contains("gpt-top"),
            "{args}"
        ),
    }
}
