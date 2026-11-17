//! The staging smoke after a merge (`smoke_run.rs`) against a fake store, a fake staging that serves `version.json` and a
//! real base checkout with the smoke command run in its own worktree. The facts are kept ([`crate::kept_facts`]) and sent by
//! the loop's fact pass, which these tests call after each pass.
use super::fix_main::{Fixture, fixture};
use super::*;
use crate::config::DeployTarget;
use crate::kept_facts::{SMOKE, StoreFacts};
use axum::routing::get;
use std::time::Instant;

/// A fake staging serving `{ "sha": <what it is set to> }` at `/version.json`; its base url and the sha it serves.
async fn staging(sha: String) -> (String, Arc<Mutex<String>>) {
    let serves = Arc::new(Mutex::new(sha));
    let seen = serves.clone();
    let app = Router::new().route(
        "/version.json",
        get(move || {
            let sha = seen.lock().unwrap().clone();
            async move { Json(json!({ "sha": sha })) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), serves)
}

struct Smoked {
    f: Fixture,
    sha: String,
    url: String,
    serves: Arc<Mutex<String>>,
}

/// A project whose staging deploys from the base branch and serves the base's head, smoked by `command`.
async fn smoked(command: &str) -> Smoked {
    let mut f = fixture(0).await;
    let sha = crate::lanes::head_sha(&f.repo).unwrap();
    let (url, serves) = staging(sha.clone()).await;
    let lp = &mut f.rig.machine.projects[0];
    lp.project.deploy.staging = Some(DeployTarget {
        branch: "main".into(),
        url: Some(url.clone()),
        smoke: Some(command.into()),
        version_path: None,
        rollout: None,
    });
    lp.project.checks.clear();
    Smoked { f, sha, url, serves }
}

fn merge_made(f: &mut Fixture) {
    f.rig.machine.projects[0].run_loop.smoke.merge_made();
}

/// One pass of the project, then the fact pass that sends what the pass kept.
async fn pass(f: &mut Fixture) {
    let m = &mut f.rig.machine;
    let shared = Shared {
        links: &m.links,
        local: &m.local,
        claim_paused_until: &mut None,
        claude_signed_out_until: &mut None,
        engine_limits: &mut Default::default(),
        stopping: false,
    };
    m.projects[0]
        .run_pass(&mut m.pool, &super::fix_main::Idle, &shared, Instant::now())
        .await;
    let lp = &m.projects[0];
    let store = StoreFacts {
        client: lp.client.clone(),
        repo: lp.repo.unwrap(),
        token: "tok".into(),
        machine: "my-pc".into(),
    };
    crate::kept_facts::shared(lp.store.dir()).flush(&store).await;
}

/// Passes until `done`, bounded by a time limit, not a count.
async fn pass_until(f: &mut Fixture, done: impl Fn(&Fixture) -> bool) {
    let limit = Instant::now() + Duration::from_secs(60);
    while !done(f) {
        assert!(Instant::now() < limit, "gave up: {:?}", f.script.lock().unwrap().log);
        pass(f).await;
        tokio::task::yield_now().await;
    }
}

/// A fixed number of passes, to see that nothing more happens.
async fn passes(f: &mut Fixture, n: usize) {
    for _ in 0..n {
        pass(f).await;
        tokio::task::yield_now().await;
    }
}

fn written(f: &Fixture) -> Vec<Value> {
    (f.script.lock().unwrap().args("fact_set"))
        .into_iter()
        .filter(|a| a["kind"] == "smoke")
        .collect()
}

fn statuses(f: &Fixture) -> Vec<String> {
    (written(f).iter())
        .map(|a| a["data"]["status"].as_str().unwrap().to_string())
        .collect()
}

fn last_is(f: &Fixture, status: &str) -> bool {
    statuses(f).last().is_some_and(|s| s == status)
}

fn triggers(f: &Fixture) -> Vec<Value> {
    f.script.lock().unwrap().args("chore_trigger")
}

/// A smoke command that waits for `gate` to exist, then runs `then`.
fn gated(gate: &std::path::Path, then: &str) -> String {
    format!("while [ ! -f '{}' ]; do sleep 0.05; done; {then}", gate.display())
}

#[cfg(unix)]
#[tokio::test]
async fn a_merge_is_smoked_once_staging_serves_it_and_each_state_is_a_fact() {
    let gate_dir = tempfile::tempdir().unwrap();
    let gate = gate_dir.path().join("go");
    let Smoked { mut f, sha, url, .. } = smoked(&gated(&gate, "echo smoke-ok")).await;
    merge_made(&mut f);
    pass_until(&mut f, |f| last_is(f, "running")).await;
    std::fs::write(&gate, "").unwrap();
    pass_until(&mut f, |f| last_is(f, "passed")).await;
    assert_eq!(statuses(&f), ["waiting", "running", "passed"]);
    let facts = written(&f);
    for fact in &facts {
        assert_eq!(
            (&fact["data"]["sha"], &fact["data"]["url"], &fact["machine"]),
            (&json!(sha), &json!(url), &json!("my-pc"))
        );
    }
    assert!(
        facts[2]["data"].get("tail").is_none(),
        "only a failure carries its tail"
    );
    assert!(triggers(&f).is_empty(), "a pass tells the store nothing more");
    let sent = f.script.lock().unwrap().log.clone();
    assert!(
        sent.iter()
            .all(|b| !["add", "lock_take"].contains(&b["op"].as_str().unwrap())),
        "no chore is added by this computer: {sent:?}"
    );
    // Not smoked again until another merge.
    passes(&mut f, 4).await;
    assert_eq!(statuses(&f).len(), 3);
}

#[cfg(unix)]
#[tokio::test]
async fn a_failed_smoke_is_a_fact_and_one_staging_smoke_trigger_the_store_decides_on() {
    let Smoked { mut f, sha, url, .. } = smoked("echo smoke-boom; exit 1").await;
    merge_made(&mut f);
    pass_until(&mut f, |f| last_is(f, "failed") && !triggers(f).is_empty()).await;
    assert_eq!(statuses(&f).last().unwrap(), "failed");
    assert!(
        written(&f).last().unwrap()["data"]["tail"]
            .as_str()
            .unwrap()
            .contains("smoke-boom")
    );
    let sent = triggers(&f);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["trigger"], "staging_smoke");
    let data = &sent[0]["data"];
    assert_eq!(
        (data["sha"].as_str(), data["url"].as_str(), data["command"].as_str()),
        (Some(sha.as_str()), Some(url.as_str()), Some("echo smoke-boom; exit 1"))
    );
    assert!(data["tail"].as_str().unwrap().contains("smoke-boom"));
    passes(&mut f, 3).await;
    assert_eq!(triggers(&f).len(), 1, "an answered trigger is not sent again");
    let s = f.script.lock().unwrap();
    assert!(
        s.args("add").is_empty() && s.args("lock_take").is_empty(),
        "{:?}",
        s.log
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_trigger_the_store_did_not_answer_is_sent_again_until_it_does_and_a_skip_ends_it() {
    let Smoked { mut f, .. } = smoked("exit 1").await;
    f.script.lock().unwrap().trigger_fails = true;
    merge_made(&mut f);
    pass_until(&mut f, |f| triggers(f).len() >= 2).await;
    {
        let mut s = f.script.lock().unwrap();
        s.trigger_fails = false;
        // The store may decline: it has a fix open.
        s.trigger = Some(json!({ "skipped": "a smoke fix is already open" }));
    }
    let before = triggers(&f).len();
    pass_until(&mut f, |f| triggers(f).len() > before).await;
    let after = triggers(&f).len();
    passes(&mut f, 3).await;
    assert_eq!(triggers(&f).len(), after, "any answer ends it");
}

#[tokio::test]
async fn a_timeout_or_a_passing_smoke_asks_for_no_fix() {
    // A deploy that never serves the commit is only a status; no 20 minutes here, the trigger is for a failed command.
    let state = crate::store::SmokeState {
        sha: "a".into(),
        status: crate::store::SmokeStatus::Timeout,
        at: "t".into(),
        url: Some("u".into()),
        tail: None,
    };
    let Smoked { mut f, .. } = smoked("true").await;
    let lp = &mut f.rig.machine.projects[0];
    lp.smoked(state);
    passes(&mut f, 3).await;
    assert!(triggers(&f).is_empty());
    assert_eq!(statuses(&f), ["timeout"]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_newer_merge_calls_off_a_wait_and_the_newer_commit_alone_is_smoked() {
    // Staging serves the commit before the first merge, so it never serves the merge.
    let Smoked { mut f, serves, .. } = smoked("echo ok").await;
    let older = super::fix_main::push_commit(&f.repo);
    merge_made(&mut f);
    pass_until(&mut f, |f| last_is(f, "waiting")).await;
    let newer = super::fix_main::push_commit(&f.repo);
    merge_made(&mut f);
    // The newer commit's wait begins only once the older one has been called off.
    pass_until(&mut f, |f| written(f).iter().any(|a| a["data"]["sha"] == json!(newer))).await;
    // Only now does staging serve it (a deploy of the newer commit has the older one's changes too).
    *serves.lock().unwrap() = newer.clone();
    pass_until(&mut f, |f| last_is(f, "passed")).await;
    let facts = written(&f);
    assert_eq!(
        facts
            .iter()
            .map(|a| a["data"]["status"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["waiting", "waiting", "running", "passed"],
        "{facts:?}"
    );
    assert_eq!(facts[0]["data"]["sha"], json!(older));
    assert!(facts[1..].iter().all(|a| a["data"]["sha"] == json!(newer)), "{facts:?}");
}

#[tokio::test]
async fn a_project_without_a_smoke_command_does_nothing_even_after_a_merge() {
    let mut f = fixture(0).await;
    merge_made(&mut f);
    passes(&mut f, 4).await;
    assert!(written(&f).is_empty());
    assert!(f.script.lock().unwrap().args("facts_get").is_empty());
    assert!(
        crate::kept_facts::shared(f.rig.machine.projects[0].store.dir())
            .owed(SMOKE)
            .is_none()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_smoke_this_computer_left_waiting_starts_again_but_another_computers_is_left_alone() {
    let Smoked { mut f, sha, .. } = smoked("echo again").await;
    f.script.lock().unwrap().facts = json!({ "smoke": { "kind": "smoke", "updatedAt": 3, "by": "me", "machine": "my-pc",
        "data": { "sha": sha, "status": "running", "at": "2026-11-16T00:00:00Z", "url": "https://x.test" } } });
    pass_until(&mut f, |f| last_is(f, "passed")).await;
    assert_eq!(statuses(&f).first().unwrap(), "waiting");

    let Smoked { mut f, sha, .. } = smoked("echo again").await;
    f.script.lock().unwrap().facts = json!({ "smoke": { "kind": "smoke", "updatedAt": 3, "by": "bob", "machine": "other-pc",
        "data": { "sha": sha, "status": "running", "at": "2026-11-16T00:00:00Z" } } });
    passes(&mut f, 4).await;
    assert!(written(&f).is_empty());

    let Smoked { mut f, sha, .. } = smoked("echo again").await;
    f.script.lock().unwrap().facts = json!({ "smoke": { "kind": "smoke", "updatedAt": 3, "by": "me", "machine": "my-pc",
        "data": { "sha": sha, "status": "passed", "at": "2026-11-16T00:00:00Z" } } });
    passes(&mut f, 4).await;
    assert!(written(&f).is_empty(), "a finished smoke isn't run again");
}

#[cfg(unix)]
#[tokio::test]
async fn through_an_outage_the_states_are_kept_and_the_newest_is_written_when_the_store_is_back() {
    let Smoked { mut f, .. } = smoked("echo ok").await;
    f.script.lock().unwrap().fact_set_fails = true;
    merge_made(&mut f);
    // Nothing is taken by the store, and the newest state is what is owed.
    let dir = f.rig.machine.projects[0].store.dir().to_path_buf();
    let owed = || {
        crate::kept_facts::shared(&dir)
            .owed(SMOKE)
            .map(|v| v["status"].as_str().unwrap().to_string())
    };
    pass_until(&mut f, |_| owed().as_deref() == Some("passed")).await;
    assert!(written(&f).iter().all(|a| a["data"]["status"] != "passed"));
    f.script.lock().unwrap().fact_set_fails = false;
    pass_until(&mut f, |f| last_is(f, "passed")).await;
    assert!(owed().is_none(), "the store took it");
}
