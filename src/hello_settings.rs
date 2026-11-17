//! What `hello` says about this computer's settings (agent-settings.md sections 2.1 and 3.2): the owner's settings, the
//! computer-only values the settings page shows, and each environment variable's name with whether it is set, never
//! its value. Also the signal that sends every project's hello again after the settings page saves.
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use serde_json::{Value, json};
use tokio::sync::watch;

use crate::computer_view::{self, ComputerContext};
use crate::config;
use crate::env::Env;
use crate::release_gate::{Inputs, ReleaseGate, Resolved};
use crate::rollout_tools;
use crate::secrets::{Secrets, Source as SecretSource};
use crate::store::MachineStore;
use crate::tags;

/// agent-settings.md section 3.2: the environment variables that set a computer-only setting. The settings page greys
/// each one out and the hello lists it as set or not.
pub const AGENT_ENV_VARS: [&str; 10] = [
    "RETASKABLE_PORT",
    "RETASKABLE_LANES",
    "RETASKABLE_CI_SLOTS",
    "RETASKABLE_POLL_SEC",
    "RETASKABLE_TAGS",
    "RETASKABLE_LOCAL_MODEL",
    "RETASKABLE_LOCAL_URL",
    "RETASKABLE_ENGINES",
    "RETASKABLE_CURSOR_MODELS",
    "RETASKABLE_NETWORK_PASSWORD",
];

/// The environment variables the agent reads, by name, that `hello.computer.secrets` lists when set, never with a
/// value (`AGENT_ENV_VARS` in `agent/src/computerView.ts`): the settings variables above, and the ones that point it at
/// a cloud or a store, a tool or a config folder.
pub const HELLO_ENV_VARS: [&str; 20] = [
    "ANTHROPIC_API_KEY",
    "RETASKABLE_CLOUD_URL",
    "RETASKABLE_BACKLOG_URL",
    "RETASKABLE_PORT",
    "RETASKABLE_LANES",
    "RETASKABLE_LOCAL_MODEL",
    "RETASKABLE_LOCAL_URL",
    "RETASKABLE_POLL_SEC",
    "RETASKABLE_TAGS",
    "RETASKABLE_CURSOR_MODELS",
    "RETASKABLE_ENGINES",
    "RETASKABLE_NETWORK_PASSWORD",
    "RETASKABLE_RELEASES",
    "RETASKABLE_NO_TRAY",
    "RETASKABLE_TUI",
    "RETASKABLE_CLAUDE",
    "RETASKABLE_CURSOR",
    "CLAUDE_CONFIG_DIR",
    "ANDROID_HOME",
    "ANDROID_SDK_ROOT",
];

/// Each variable's name and whether it is set.
pub fn env_vars_json(env: &Env) -> Value {
    Value::Object(
        AGENT_ENV_VARS
            .iter()
            .map(|name| ((*name).to_string(), Value::Bool(env.get(name).is_some())))
            .collect(),
    )
}

/// `computer.releases`: `{ on, why? }`.
pub fn releases_json(r: &Resolved) -> Value {
    match &r.why {
        Some(why) => json!({ "on": r.on, "why": why }),
        None => json!({ "on": r.on }),
    }
}

/// Where the hello reads the settings from, and the signal that a save fired. Clones share the signal.
#[derive(Clone)]
pub struct HelloSettings {
    saved: Arc<watch::Sender<u64>>,
    source: Option<Source>,
    /// The saved secrets, for their names and source in the hello (given once at start; clones share it).
    secrets: Arc<OnceLock<Arc<Secrets>>>,
    /// The port default, build and links (`computer_view`), given once at start.
    context: Arc<OnceLock<ComputerContext>>,
    /// The tags this computer finds for itself, looked up on the first hello (`nvidia-smi` runs once).
    found: Arc<OnceLock<Vec<String>>>,
    /// Whether this computer may release each project (`release_gate`), shared by every connection.
    gate: ReleaseGate,
}

#[derive(Clone)]
struct Source {
    dir: PathBuf,
    env: Env,
}

impl std::fmt::Debug for HelloSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HelloSettings(..)")
    }
}

impl Default for HelloSettings {
    fn default() -> Self {
        Self {
            saved: Arc::new(watch::Sender::new(0)),
            source: None,
            secrets: Arc::default(),
            context: Arc::default(),
            found: Arc::default(),
            gate: ReleaseGate::default(),
        }
    }
}

impl PartialEq for HelloSettings {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl HelloSettings {
    /// Reads the config in `dir` with `env` over it, as the settings page does.
    pub fn new(dir: PathBuf, env: Env) -> Self {
        Self {
            source: Some(Source { dir, env }),
            ..Self::default()
        }
    }

    /// Has the hello say where each secret comes from (agent-settings.md section 3.3), by name only.
    pub fn set_secrets(&self, secrets: Arc<Secrets>) {
        let _ = self.secrets.set(secrets);
    }

    /// Has the hello carry the port default, the build and each link's state (`computer.port`, `cloud`, the links).
    pub fn set_context(&self, context: ComputerContext) {
        let _ = self.context.set(context);
    }

    /// Whether this computer may release each project, and the hub's word on the owner's role.
    pub fn gate(&self) -> &ReleaseGate {
        &self.gate
    }

    /// Wakes on a link's state changing (a link started, approved or ended), so the hello says it.
    pub fn subscribe_links(&self) -> watch::Receiver<u64> {
        match self.context.get() {
            Some(c) => c.links.subscribe(),
            // Nothing says where the links are: a receiver that never changes.
            None => watch::channel(0).1,
        }
    }

    /// Whether this computer may release `project` and why not (`ReleaseGate.resolve`): `Off` for a project it doesn't
    /// have, as the TypeScript agent says it. `None` when nothing says where the config is, or it won't read.
    pub fn releases(&self, project: &str) -> Option<Resolved> {
        let s = self.source.as_ref()?;
        let cfg = config::load(&s.dir).ok()?;
        let project_cfg = cfg.projects.iter().find(|p| p.id_or_derived() == project);
        let Some(p) = project_cfg else {
            return Some(self.gate.resolve(
                project,
                &Inputs {
                    switch: config::releases_switch(&cfg, &s.env),
                    repo: "",
                    rollout: false,
                    missing_tools: None,
                },
                Instant::now(),
            ));
        };
        let on_path = |program: &str| rollout_tools::on_path(program, &s.env);
        let input = Inputs {
            switch: config::releases_switch(&cfg, &s.env),
            repo: &p.github_repo,
            rollout: rollout_tools::rollout_of(&p.deploy).is_some(),
            missing_tools: rollout_tools::missing(&p.deploy, &s.env, &on_path),
        };
        Some(self.gate.resolve(project, &input, Instant::now()))
    }

    /// Whether `project`'s rollout commands run without Approve (`ReleaseGate.rolloutUnasked`): the switch resolves on
    /// and the hub has said the owner's role. False until the hub says it.
    pub fn rollout_unasked(&self, project: &str) -> bool {
        self.gate.role(project).is_some() && self.releases(project).is_some_and(|r| r.on)
    }

    /// Makes every open connection send a fresh hello.
    pub fn fire(&self) {
        self.saved.send_modify(|n| *n += 1);
    }

    /// What `project` has cost today on this computer, and the machine's `budget.dailyUsd` (0 when unset).
    pub fn spend_today(&self, project: &str) -> (f64, f64) {
        let Some(dir) = self.dir() else {
            return (0.0, 0.0);
        };
        let spent = MachineStore::load(dir).map_or(0.0, |m| m.spend_today(project));
        let daily = config::load(dir).map_or(0.0, |c| c.machine.budget.daily_usd);
        (spent, daily)
    }

    /// The directory holding `config.json`, when something says where it is.
    pub fn dir(&self) -> Option<&Path> {
        self.source.as_ref().map(|s| s.dir.as_path())
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.saved.subscribe()
    }

    /// The hello's `computer` (`ComputerView`, `computer_view::view`): the computer's settings, and `secrets: [{ key,
    /// state, kind }]` by name only, listed when set (`state: "set"`, `kind: "env"` when the environment sets it,
    /// `"key"` when it is saved here), then each link (`kind: "link"`), as the TypeScript agent does. Never a value, a
    /// fingerprint or ciphertext. `None` when nothing says where the config is, or it won't read: the site drops a
    /// malformed `computer` and wants `lanes`.
    pub fn computer(&self, project: &str) -> Option<Value> {
        let s = self.source.as_ref()?;
        let cfg = config::load(&s.dir).ok()?;
        let mut secrets: Vec<Value> = HELLO_ENV_VARS
            .iter()
            .filter(|name| s.env.get(name).is_some())
            .map(|name| json!({ "key": name, "state": "set", "kind": "env" }))
            .collect();
        if let Some(saved) = self.secrets.get() {
            for t in saved.states() {
                let kind = match t.source {
                    SecretSource::Env => "env",
                    SecretSource::Saved => "key",
                    SecretSource::Unset | SecretSource::Unreadable => continue,
                };
                // The site learns the known secrets only, never the names of variables a person added; one the
                // environment list already names (`ANTHROPIC_API_KEY`) is said once.
                if t.def.is_some() && !HELLO_ENV_VARS.contains(&t.name.as_str()) {
                    secrets.push(json!({ "key": t.name, "state": "set", "kind": kind }));
                }
            }
        }
        let context = self.context.get();
        if let Some(c) = context {
            secrets.extend(computer_view::links(c, &cfg, &s.env));
        }
        let found = self
            .found
            .get_or_init(|| tags::found_tags(&tags::machine_facts(&s.env)));
        let machine = MachineStore::load(&s.dir).ok();
        let mut view = computer_view::view(&cfg, &s.env, machine.as_ref(), context, found);
        view.insert("secrets".into(), Value::Array(secrets));
        if let Some(r) = self.releases(project) {
            view.insert("releases".into(), releases_json(&r));
        }
        Some(Value::Object(view))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_vars_json_lists_every_name_with_whether_it_is_set_never_the_value() {
        let env = Env::from_pairs([("RETASKABLE_PORT", "4567"), ("RETASKABLE_NETWORK_PASSWORD", "hunter2")]);
        let v = env_vars_json(&env);
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), AGENT_ENV_VARS.len());
        for name in AGENT_ENV_VARS {
            let set = name == "RETASKABLE_PORT" || name == "RETASKABLE_NETWORK_PASSWORD";
            assert_eq!(obj[name], Value::Bool(set), "{name}");
        }
        assert!(!v.to_string().contains("hunter2"));
    }

    #[test]
    fn env_vars_json_is_all_false_for_an_empty_env() {
        let v = env_vars_json(&Env::empty());
        assert!(v.as_object().unwrap().values().all(|b| *b == Value::Bool(false)));
    }

    /// shared/cloud.ts `computerHello`: `lanes` a number, `secrets` at most 60 of `{ key (<= 300), state (<= 20) }`
    /// (loose, so `kind` rides along). A `computer` that doesn't fit is dropped silently by the site. The rest of the
    /// view is `computer_view`'s tests and the hub contract's.
    #[tokio::test]
    async fn the_hello_computer_lists_secrets_by_name_and_state_only() {
        use crate::secrets::fakes::{FakeHost, MemStore};
        const PLANTED: &str = "ghp_PLANTED_hello_value_42";
        let dir = tempfile::tempdir().unwrap();
        let host = Arc::new(FakeHost::with(&[("GITLAB_TOKEN", "glpat-from-env")]));
        let secrets = Secrets::new(
            dir.path(),
            &Env::empty(),
            host.clone(),
            Some(Arc::new(MemStore::default())),
            Arc::new(MemStore::default()),
        );
        secrets.apply();
        let plan = secrets
            .plan(
                json!({ "GH_TOKEN": PLANTED, "ANTHROPIC_API_KEY": null })
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        secrets.commit(plan).unwrap();
        let env = Env::from_pairs([("RETASKABLE_NETWORK_PASSWORD", "hunter2")]);
        let h = HelloSettings::new(dir.path().to_path_buf(), env);
        h.set_secrets(Arc::new(secrets));

        let computer = h.computer("a").unwrap();
        assert!(computer["lanes"].is_number());
        assert!(computer["tags"]["found"].is_array());
        assert_eq!(computer["port"], crate::config::DEFAULT_PORT);
        let rows = computer["secrets"].as_array().unwrap();
        assert!(rows.len() <= 60);
        let mut seen = Vec::new();
        for row in rows {
            let keys: Vec<&String> = row.as_object().unwrap().keys().collect();
            assert_eq!(keys, ["key", "kind", "state"], "{row}");
            assert!(row["key"].as_str().unwrap().len() <= 300);
            assert_eq!(row["state"], "set");
            seen.push((row["key"].as_str().unwrap(), row["kind"].as_str().unwrap()));
        }
        assert_eq!(
            seen,
            [
                ("RETASKABLE_NETWORK_PASSWORD", "env"),
                ("GH_TOKEN", "key"),
                ("GITLAB_TOKEN", "env"),
            ]
        );
        let text = computer.to_string();
        for never in [
            PLANTED,
            "glpat-from-env",
            "hunter2",
            &crate::secrets::fingerprint(PLANTED),
        ] {
            assert!(!text.contains(never), "{never} in {text}");
        }
        assert!(HelloSettings::default().computer("a").is_none());
    }

    #[tokio::test]
    async fn without_a_source_nothing_is_read() {
        let h = HelloSettings::default();
        assert!(h.dir().is_none());
        assert_eq!(h.spend_today("any"), (0.0, 0.0));
    }

    #[test]
    fn fire_wakes_subscribers_of_every_clone() {
        let h = HelloSettings::default();
        let clone = h.clone();
        let mut rx = h.subscribe();
        assert!(!rx.has_changed().unwrap());
        clone.fire();
        assert!(rx.has_changed().unwrap());
        assert_eq!(*rx.borrow_and_update(), 1);
        h.fire();
        assert_eq!(*rx.borrow_and_update(), 2);
    }

    #[test]
    fn all_hello_settings_compare_equal() {
        assert_eq!(HelloSettings::default(), HelloSettings::default());
    }

    /// `AGENT_ENV_VARS` in `agent/src/computerView.ts`: each name that is set is listed, in the list's order, with
    /// `kind: "env"`; one that is also a known secret (`ANTHROPIC_API_KEY`) is said once, and never a value.
    #[tokio::test]
    async fn the_hello_lists_every_environment_variable_the_agent_reads_that_is_set_once_and_by_name() {
        use crate::secrets::fakes::{FakeHost, MemStore};
        assert_eq!(HELLO_ENV_VARS.len(), 20);
        let dir = tempfile::tempdir().unwrap();
        let secrets = Secrets::new(
            dir.path(),
            &Env::empty(),
            Arc::new(FakeHost::with(&[])),
            Some(Arc::new(MemStore::default())),
            Arc::new(MemStore::default()),
        );
        secrets.apply();
        let plan = secrets
            .plan(json!({ "ANTHROPIC_API_KEY": "sk-ant-PLANTED" }).as_object().unwrap())
            .unwrap();
        secrets.commit(plan).unwrap();
        let env = Env::from_pairs([
            ("ANDROID_SDK_ROOT", "/sdk"),
            ("RETASKABLE_RELEASES", "0"),
            ("CLAUDE_CONFIG_DIR", "/c"),
            ("RETASKABLE_CLOUD_URL", "https://x.test"),
            ("ANTHROPIC_API_KEY", "sk-ant-PLANTED"),
            ("SOMETHING_ELSE", "x"),
        ]);
        let h = HelloSettings::new(dir.path().to_path_buf(), env);
        h.set_secrets(Arc::new(secrets));
        let computer = h.computer("a").unwrap();
        let keys: Vec<&str> = computer["secrets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["key"].as_str().unwrap())
            .collect();
        assert_eq!(
            keys,
            [
                "ANTHROPIC_API_KEY",
                "RETASKABLE_CLOUD_URL",
                "RETASKABLE_RELEASES",
                "CLAUDE_CONFIG_DIR",
                "ANDROID_SDK_ROOT"
            ]
        );
        assert!(!computer.to_string().contains("PLANTED"));
    }

    #[test]
    fn rollout_commands_run_unasked_only_once_the_hub_has_said_the_role_and_the_answer_is_on() {
        let dir = tempfile::tempdir().unwrap();
        let on = HelloSettings::new(dir.path().to_path_buf(), Env::empty());
        on.gate().record("p", true, Instant::now());
        // Answers on for an older hub, but the rollout commands wait for the owner's role.
        assert!(!on.rollout_unasked("p"));
        on.gate().set_role("p", Some(crate::release_gate::Role::Member));
        assert!(!on.rollout_unasked("p"));
        on.gate().set_role("p", Some(crate::release_gate::Role::Admin));
        assert!(on.rollout_unasked("p"));
        let off = HelloSettings::new(
            dir.path().to_path_buf(),
            Env::from_pairs([("RETASKABLE_RELEASES", "0")]),
        );
        off.gate().record("p", true, Instant::now());
        off.gate().set_role("p", Some(crate::release_gate::Role::Admin));
        assert!(!off.rollout_unasked("p"));
    }
}
