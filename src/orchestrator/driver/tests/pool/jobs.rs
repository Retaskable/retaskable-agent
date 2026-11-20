//! Jobs as pool tasks (`driver/job_tasks.rs`): `can` in `health`, a `task.job` run through the host's tools and ended
//! with its raw result, a refusal in words, and the end kept like any other.
use std::collections::BTreeMap;

use futures_util::future::BoxFuture;

use super::*;
use crate::config::DeployTarget;
use crate::jobs::host::{JobHost, JobRole, JobSession, JobSessionConfig};
use crate::jobs::probes::{Method, ProbeResponse, TlsAnswer, TlsVersion};

/// A host whose tools answer at once with a plain page; it records the urls the probes asked for.
#[derive(Default)]
struct Tools {
    asked: Mutex<Vec<String>>,
}

impl JobHost for Tools {
    fn probe_fetch(
        &self,
        url: String,
        _method: Method,
        headers: Vec<(String, String)>,
    ) -> BoxFuture<'static, Result<ProbeResponse, String>> {
        self.asked.lock().unwrap().push(format!("{url} {}", headers.len()));
        Box::pin(async {
            Ok(ProbeResponse {
                status: 200,
                headers: BTreeMap::new(),
                cookies: vec![],
                body: String::new(),
            })
        })
    }

    fn probe_tls(&self, _host: String, _port: u16, _version: Option<TlsVersion>) -> BoxFuture<'static, TlsAnswer> {
        Box::pin(async {
            TlsAnswer::Failed {
                local: true,
                error: "no handshake in a test".into(),
            }
        })
    }

    fn probe_wait(&self, _d: Duration) -> BoxFuture<'static, ()> {
        Box::pin(async {})
    }
}

/// A planner session that answers `groups` at once, noting the prompt it was given.
struct Planner {
    config: JobSessionConfig,
    groups: Value,
    prompts: Arc<Mutex<Vec<String>>>,
}

impl JobSession for Planner {
    fn config(&self) -> &JobSessionConfig {
        &self.config
    }

    fn run(
        &self,
        spec: crate::engines::SessionSpec,
    ) -> BoxFuture<'static, anyhow::Result<crate::engines::SessionResult>> {
        self.prompts.lock().unwrap().push(spec.prompt.clone());
        let answer = self.groups.clone();
        Box::pin(async move { Ok(crate::engines::engine::test_result(true, "success", Some(answer), None)) })
    }
}

/// A host whose planner answers `groups` for a `Group` session.
struct Grouping {
    groups: Value,
    prompts: Arc<Mutex<Vec<String>>>,
    roles: Arc<Mutex<Vec<JobRole>>>,
}

impl JobHost for Grouping {
    fn session(
        &self,
        _project: &crate::config::ProjectConfig,
        role: JobRole,
    ) -> BoxFuture<'static, Result<Box<dyn JobSession>, String>> {
        self.roles.lock().unwrap().push(role);
        let planner = Planner {
            config: JobSessionConfig {
                cwd: ".".into(),
                budget_usd: 0.5,
                model: "sonnet".into(),
                local: None,
                engine: None,
                cursor: None,
                sandbox: false,
                shell: true,
            },
            groups: self.groups.clone(),
            prompts: self.prompts.clone(),
        };
        Box::pin(async move { Ok(Box::new(planner) as Box<dyn JobSession>) })
    }
}

/// Changes the project's settings as config.json would have them (the pass rebuilds `project` from `file_project`).
pub(super) fn set(rig: &mut Rig, f: impl Fn(&mut crate::config::ProjectConfig)) {
    f(&mut rig.lp(0).file_project);
    f(&mut rig.lp(0).project);
}

pub(super) fn job_task(id: &str, kind: &str, args: Value) -> Value {
    let mut t = task(id, &format!("job:{kind}"));
    t["job"] = json!({ "kind": kind, "day": "2026-11-17", "args": args });
    t
}

/// Passes until the store has heard `n` task ends (jobs run in their own task), without a fixed wait.
pub(super) async fn until_ended(rig: &mut Rig, asked: &Asked, n: usize) -> Vec<Value> {
    for _ in 0..200 {
        rig.lp(0).pool.every = Some(Duration::ZERO);
        rig.machine.pass().await;
        let ends = sent(asked, "task_end");
        if ends.len() >= n {
            return ends;
        }
        tokio::task::yield_now().await;
    }
    panic!("the jobs didn't end: {:?}", sent(asked, "task_end"));
}

#[tokio::test]
async fn health_says_what_this_computer_can_run_only_with_a_job_host() {
    let (mut rig, _fake, asked, _plan) = live(vec![]).await;
    rig.machine.pass().await;
    assert_eq!(sent(&asked, "health")[0]["can"], json!([]), "no host, no jobs");
    rig.machine.jobs = Some(Arc::new(Tools::default()));
    set(&mut rig, |p| p.repo_path = "/work/p".into());
    set(&mut rig, |p| p.scan.probes = true);
    set(&mut rig, |p| {
        p.deploy.staging = Some(serde_json::from_value(json!({ "url": "https://staging.example.com" })).unwrap())
    });
    rig.machine.links.env = Env::from_pairs([("SCAN_TOKEN_STAGING_EXAMPLE_COM", "a-token")]);
    rig.lp(0).pool.every = Some(Duration::ZERO);
    rig.machine.pass().await;
    let can = sent(&asked, "health").last().unwrap()["can"].clone();
    assert_eq!(can, json!(["chrome", "repo", "engine", "scanToken"]));
    assert!(!sent(&asked, "health").last().unwrap().to_string().contains("a-token"));
}

#[tokio::test]
async fn a_probes_job_runs_with_the_saved_token_and_ends_with_its_raw_result_and_no_secret() {
    let tools = Arc::new(Tools::default());
    let tasks = vec![job_task(
        "task-5",
        "probes",
        json!({ "url": "https://staging.example.com" }),
    )];
    let (mut rig, _fake, asked, _plan) = live(tasks).await;
    rig.machine.jobs = Some(tools.clone());
    set(&mut rig, |p| p.repo_path = "/work/p".into());
    set(&mut rig, |p| {
        p.scan = serde_json::from_value(json!({ "probes": true, "signedInPaths": ["/api/me"] })).unwrap()
    });
    set(&mut rig, |p| {
        p.deploy.staging = Some(DeployTarget {
            url: Some("https://staging.example.com".into()),
            ..serde_json::from_value(json!({})).unwrap()
        })
    });
    rig.machine.links.env = Env::from_pairs([("SCAN_TOKEN_STAGING_EXAMPLE_COM", "a-token")]);
    let ends = until_ended(&mut rig, &asked, 1).await;
    assert_eq!(ends[0]["id"], "task-5");
    assert_eq!(ends[0]["ok"], true, "{}", ends[0]);
    assert_eq!(ends[0]["result"]["day"], "2026-11-17");
    assert!(ends[0]["result"]["results"].as_array().is_some_and(|r| !r.is_empty()));
    assert!(
        !ends[0].to_string().contains("a-token"),
        "a secret is never in a result"
    );
    // The signed-in path was read with the token (one header: Authorization), the others without.
    assert!(tools.asked.lock().unwrap().iter().any(|u| u.ends_with("/api/me 1")));
}

#[tokio::test]
async fn a_job_the_computer_can_not_run_is_refused_in_words() {
    let tasks = vec![
        job_task("task-6", "lighthouse", json!({})),
        job_task("task-7", "nope", json!({})),
        job_task("task-8", "vet", json!({})),
    ];
    let (mut rig, _fake, asked, _plan) = live(tasks).await;
    rig.machine.jobs = Some(Arc::new(Tools::default()));
    let ends = until_ended(&mut rig, &asked, 3).await;
    let by = |id: &str| ends.iter().find(|e| e["id"] == id).unwrap().clone();
    assert_eq!(by("task-6")["refused"]["why"], "Lighthouse isn't on for this project.");
    assert_eq!(by("task-7")["refused"]["why"], "This computer doesn't run nope jobs.");
    // A session job on a host with no engine fails in words, never silently.
    assert_eq!(by("task-8")["ok"], false);
    assert!(by("task-8")["note"].as_str().unwrap().starts_with("The vet run failed"));
}

#[tokio::test]
async fn a_running_job_is_reported_running_and_its_end_is_kept_until_the_store_takes_it() {
    let tasks = vec![job_task("task-9", "deprecations", json!({}))];
    let (mut rig, _fake, asked, plan) = live(tasks).await;
    rig.machine.jobs = Some(Arc::new(Tools::default()));
    plan.lock().unwrap().end_down = true;
    rig.lp(0).pool.every = Some(Duration::ZERO);
    rig.machine.pass().await;
    for _ in 0..200 {
        rig.machine.pass().await;
        if rig.lp(0).store.task_ends_owed().iter().any(|o| o.end.id == "task-9") {
            break;
        }
        tokio::task::yield_now().await;
    }
    let owed = rig.lp(0).store.task_ends_owed();
    assert_eq!(owed[0].end.id, "task-9");
    assert!(
        owed[0].end.refused.is_some() || owed[0].end.ok,
        "the job's own answer is what is owed"
    );
    assert_eq!(
        sent(&asked, "health").last().unwrap()["running"],
        json!([]),
        "ended jobs aren't running"
    );
    plan.lock().unwrap().end_down = false;
    rig.lp(0).pool.retry_owed_now();
    until_ended(&mut rig, &asked, 2).await;
    assert!(rig.lp(0).store.task_ends_owed().is_empty());
}

#[tokio::test]
async fn a_playtest_group_job_runs_the_planner_over_its_entries_and_answers_the_groups_with_no_lock_or_timer_of_the_agents()
 {
    let entries = json!([
        { "key": "a-1", "itemId": "a", "title": "A", "what": "Look", "steps": ["Open it"] },
        { "key": "b-2", "itemId": "b", "title": "B", "what": "Try", "steps": [] },
    ]);
    let tasks = vec![job_task(
        "task-g",
        "playtest_group",
        json!({ "signature": "a-1|b-2", "entries": entries }),
    )];
    let (mut rig, _fake, asked, _plan) = live(tasks).await;
    let (prompts, roles) = (Arc::new(Mutex::new(Vec::new())), Arc::new(Mutex::new(Vec::new())));
    let groups = json!({ "groups": [{ "title": "Build", "why": "same place", "keys": ["a-1", "b-2"] }] });
    rig.machine.jobs = Some(Arc::new(Grouping {
        groups: groups.clone(),
        prompts: prompts.clone(),
        roles: roles.clone(),
    }));
    set(&mut rig, |p| p.repo_path = "/work/p".into());
    let ends = until_ended(&mut rig, &asked, 1).await;
    assert_eq!(ends[0]["ok"], true, "{}", ends[0]);
    assert_eq!(ends[0]["result"], groups, "the planner's groups, as it gave them");
    assert_eq!(*roles.lock().unwrap(), [JobRole::Group]);
    let prompt = prompts.lock().unwrap()[0].clone();
    assert!(prompt.contains("key: a-1") && prompt.contains("key: b-2"), "{prompt}");
    for op in ["tryouts_list", "tryout_groups", "chore_lock", "lock_take"] {
        assert!(
            sent(&asked, op).is_empty(),
            "the store asked for the grouping; {op} is not the agent's to send"
        );
    }
}
