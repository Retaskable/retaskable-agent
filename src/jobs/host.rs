//! What a job needs of the machine that the driver can't give plainly, so the driver's tests can fake it: the
//! read-only session a vet, triage or release-notes job runs, and the tools a job runs outside itself (Lighthouse, the
//! probes' requests and TLS, the browser sweep). The real host is `orchestrator::job_host::ClaudeJobs`; each tool has
//! its real default here, a fake overrides it.
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use serde_json::Value;

use super::a11y_sweep::{SweepRun, Sweeper};
use super::lighthouse::{LighthouseRunner, NpxLighthouse};
use super::probes::{Method, ProbeDeps, ProbeResponse, RealProbeDeps, TlsAnswer, TlsVersion};
use crate::config::ProjectConfig;
use crate::engines::engine::{CursorOptions, LocalTarget};
use crate::engines::{EngineId, EphemeralSessionConfig, SessionResult, SessionSpec};
use crate::lighthouse::run::{LighthouseOutcome, Preset};

/// Which session a job runs (the roles, tiers and budgets of `jobTasks.ts`'s runners).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobRole {
    /// Triage's first look: the triager's engine, the chore tier, `budget.reviewerUsd`.
    TriageLook,
    /// Triage's check of a clear suggestion: the same, on the top tier.
    TriageCheck,
    /// Vetting suggestions: the writer's engine on the scouts' tier (haiku, or the local model), `budget.scoutUsd`.
    Vet,
    /// Release notes: the writer's engine, `budget.releaseNotesUsd`.
    Release,
}

/// A job session's engine, model, budget, checkout and sandbox, owned (the bundle [`EphemeralSessionConfig`] borrows).
#[derive(Debug, Clone)]
pub struct JobSessionConfig {
    pub cwd: PathBuf,
    pub budget_usd: f64,
    pub model: String,
    pub local: Option<LocalTarget>,
    pub engine: Option<EngineId>,
    pub cursor: Option<CursorOptions>,
    pub sandbox: bool,
    pub shell: bool,
}

impl JobSessionConfig {
    pub fn ephemeral(&self) -> EphemeralSessionConfig<'_> {
        EphemeralSessionConfig {
            cwd: &self.cwd,
            budget_usd: self.budget_usd,
            model: self.model.clone(),
            local: self.local.clone(),
            engine: self.engine,
            cursor: self.cursor.clone(),
            sandbox: self.sandbox,
            shell: self.shell,
        }
    }
}

/// One read-only session a job runs, listed on the Agents tab while it runs and counted against the computer's
/// sessions. Dropping it frees the local model's slot and the session count.
pub trait JobSession: Send + Sync {
    fn config(&self) -> &JobSessionConfig;
    /// Runs `spec` (built from [`Self::config`]) to its end.
    fn run(&self, spec: SessionSpec) -> BoxFuture<'static, Result<SessionResult>>;
}

pub trait JobHost: Send + Sync {
    /// The session for `role` on `project`, or why this computer can't run one (no engine, signed out).
    fn session(
        &self,
        _project: &ProjectConfig,
        _role: JobRole,
    ) -> BoxFuture<'static, Result<Box<dyn JobSession>, String>> {
        Box::pin(async { Err("this computer has no engine for it".to_string()) })
    }

    /// One Lighthouse run (`npx lighthouse`), off the async threads.
    fn lighthouse(&self, url: String, preset: Preset, output: PathBuf) -> BoxFuture<'static, LighthouseOutcome> {
        Box::pin(async move { NpxLighthouse.run(&url, preset, &output).await })
    }

    /// The probes' GET, HEAD or OPTIONS (never anything else), redirects not followed.
    fn probe_fetch(
        &self,
        url: String,
        method: Method,
        headers: Vec<(String, String)>,
    ) -> BoxFuture<'static, Result<ProbeResponse, String>> {
        Box::pin(async move {
            let headers: Vec<(&str, String)> = headers.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
            RealProbeDeps::new().fetch(&url, method, &headers).await
        })
    }

    /// A TLS handshake for the probes' version and certificate checks.
    fn probe_tls(&self, host: String, port: u16, version: Option<TlsVersion>) -> BoxFuture<'static, TlsAnswer> {
        Box::pin(async move { RealProbeDeps::new().tls(&host, port, version).await })
    }

    fn probe_wait(&self, d: Duration) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(d))
    }

    fn probe_now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    /// The accessibility sweep of `routes` on `base`, in a headless Chrome. A computer with none answers the
    /// no-Chrome failure, which the job turns into its refusal.
    fn sweep(&self, base: String, routes: Vec<String>, prefs: Option<Value>) -> BoxFuture<'static, SweepRun> {
        let _ = (&base, &routes, &prefs);
        Box::pin(async {
            SweepRun::Failed {
                no_chrome: true,
                error: "this build of the agent has no browser to sweep with".to_string(),
            }
        })
    }
}

/// The host's tools as the job functions' own dependency traits.
pub struct Tools<'a>(pub &'a dyn JobHost);

impl LighthouseRunner for Tools<'_> {
    fn run(&self, url: &str, preset: Preset, output: &Path) -> impl Future<Output = LighthouseOutcome> + Send {
        self.0.lighthouse(url.to_string(), preset, output.to_path_buf())
    }
}

impl ProbeDeps for Tools<'_> {
    fn fetch(
        &self,
        url: &str,
        method: Method,
        headers: &[(&str, String)],
    ) -> impl Future<Output = Result<ProbeResponse, String>> + Send {
        let headers = headers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect();
        self.0.probe_fetch(url.to_string(), method, headers)
    }

    fn tls(&self, host: &str, port: u16, version: Option<TlsVersion>) -> impl Future<Output = TlsAnswer> + Send {
        self.0.probe_tls(host.to_string(), port, version)
    }

    fn wait(&self, d: Duration) -> impl Future<Output = ()> + Send {
        self.0.probe_wait(d)
    }

    fn now(&self) -> DateTime<Utc> {
        self.0.probe_now()
    }
}

impl Sweeper for Tools<'_> {
    fn sweep(
        &self,
        base: &str,
        routes: &[String],
        sample_prefs: Option<&Value>,
    ) -> impl Future<Output = SweepRun> + Send {
        self.0.sweep(base.to_string(), routes.to_vec(), sample_prefs.cloned())
    }
}
