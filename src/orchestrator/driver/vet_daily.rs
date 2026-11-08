//! The daily vet pass (docs/specs/agent/06.7-chores-never-idle.md, "Vetting visitors' suggestions"). Each minute
//! [`super::Machine::vet_rounds`] starts [`run`] for a project not already running one. Behind the
//! `chore-vetting-suggestions` flag, with `vetSuggestions.enabled` and a shared backlog link, it asks the store
//! whether the `vet` job is due, takes that day's `vet-<day>` lock, judges the waiting suggestions in one
//! read-only session and sends each verdict. It holds no lane, calls no `run_take` and counts toward nothing.
use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use super::playtest_round::Job as PlaytestJob;
use crate::backlog::suggestions::{Verdict, VerdictSend, VerdictSent};
use crate::engines::claude::sandbox_enabled;
use crate::engines::cursor::with_project;
use crate::engines::{
    EngineId, EphemeralSessionConfig, Role, SessionResult, SessionSpec, engine_for, engine_of, place, run_session,
};
use crate::flags::Flags;
use crate::ideas::Ideas;
use crate::orchestrator::session::watch_for;
use crate::vet::{Judged, PastVerdict, VetRecord, Vetter, plan, take_batch};

/// The job's name in the store's `jobs_due`.
const JOB: &str = "vet";
/// How many past verdicts the next prompt is given.
const PAST_KEPT: usize = 30;
const TEMPLATE: &str = include_str!("../../../prompts/vet-suggestions.md");

/// What the last run did: the day and its counts by verdict name.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Last {
    pub day: String,
    pub counts: BTreeMap<String, usize>,
}

#[derive(Default)]
pub(super) struct State {
    due: super::ideas_daily::Due,
    pub(crate) last: Option<Last>,
    /// The last [`PAST_KEPT`] verdicts given, oldest first.
    pub(crate) recent: Vec<PastVerdict>,
    /// A run changed `last`/`recent` since they were last written to the state file.
    pub(super) dirty: bool,
}

impl AsMut<super::ideas_daily::Due> for State {
    fn as_mut(&mut self) -> &mut super::ideas_daily::Due {
        &mut self.due
    }
}

impl State {
    /// The memory a saved record restores (a restart keeps the history).
    pub(super) fn from_record(record: Option<VetRecord>) -> Self {
        let mut st = Self::default();
        if let Some(r) = record {
            let over = r.recent.len().saturating_sub(PAST_KEPT);
            st.recent = r.recent[over..].to_vec();
            st.last = Some(Last {
                day: r.day,
                counts: r.counts,
            });
        }
        st
    }

    /// What to write to the state file, once a run has been made.
    pub(super) fn record(&self) -> Option<VetRecord> {
        self.last.as_ref().map(|l| VetRecord {
            day: l.day.clone(),
            counts: l.counts.clone(),
            recent: self.recent.clone(),
        })
    }

    /// Lets the next minute ask the store again at once (tests).
    #[cfg(test)]
    pub(super) fn forget_asker(&mut self) {
        self.due.forget_asker();
    }
}

pub(super) struct Daily {
    pub base: PlaytestJob,
    pub flags: Arc<Flags>,
    pub state: Arc<Mutex<State>>,
}

fn name(v: Verdict) -> &'static str {
    match v {
        Verdict::Promising => "promising",
        Verdict::Duplicate => "duplicate",
        Verdict::NotPromising => "not_promising",
        Verdict::Spam => "spam",
    }
}

/// Runs today's pass when everything allows it and the store says it's due; does nothing otherwise (fail closed).
pub(super) async fn run(daily: Daily) {
    let engine = engine_for(
        &daily.base.engine_choice,
        daily.base.project.engines.as_ref(),
        Role::Scout,
        None,
    );
    let placed = match place(engine, Role::Scout, &daily.base.env, &daily.base.cursor_models) {
        Ok(placed) => placed,
        Err(why) => {
            tracing::info!("{}: vet pass skipped: {why}", daily.base.id);
            return;
        }
    };
    let (bin, program) = (placed.bin.clone(), placed.engine);
    let quiet = watch_for(&daily.base.project, program);
    let (sessions, id) = (daily.base.sessions.clone(), daily.base.id.clone());
    run_with(daily, Some(placed), |spec| async move {
        let engine = engine_of(Some(program))?;
        let listed = sessions.register_spec(&id, "vet", None, Role::Scout, &spec);
        listed
            .scope(run_session(engine.as_ref(), &bin, &spec, "vet", None, quiet))
            .await
    })
    .await
}

/// [`run`] with the session's runner given.
pub(super) async fn run_with<F, Fut>(daily: Daily, placed: Option<crate::engines::Placement>, runner: F)
where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let Daily { base, flags, state } = daily;
    if !flags.on("chore-vetting-suggestions").await || !base.project.vet_suggestions.enabled {
        return;
    }
    let Some((repo, token, day)) = super::ideas_daily::due_day(&base, &state, JOB, "vet pass").await else {
        return;
    };
    let lock = format!("vet-{day}");
    if let Some(yesterday) = chrono::NaiveDate::parse_from_str(&day, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.pred_opt())
    {
        let old = format!("vet-{yesterday}");
        if let Err(e) = base.client.lock(repo, Some(&token), &old, false).await {
            tracing::warn!("{}: dropping {old} failed: {e}", base.id);
        }
    }
    match base.client.lock(repo, Some(&token), &lock, true).await {
        Ok(a) if a.by.as_deref() == Some(base.machine.as_str()) => {}
        Ok(_) => {
            tracing::info!("{}: vet pass skipped: another computer holds {lock}", base.id);
            return;
        }
        Err(e) => {
            tracing::warn!("{}: vet pass skipped: taking {lock}: {e}", base.id);
            return;
        }
    }
    pass(&base, &state, &day, repo, &token, placed, runner).await;
    if let Err(e) = base.client.lock(repo, Some(&token), &lock, false).await {
        tracing::warn!("{}: dropping {lock} failed: {e}", base.id);
    }
}

async fn pass<F, Fut>(
    base: &PlaytestJob,
    state: &Mutex<State>,
    day: &str,
    repo: u64,
    token: &str,
    placed: Option<crate::engines::Placement>,
    runner: F,
) where
    F: FnOnce(SessionSpec) -> Fut,
    Fut: Future<Output = Result<SessionResult>>,
{
    let waiting = match base.client.suggestions_ask(repo, Some(token)).await {
        Ok(all) => all,
        Err(e) => {
            tracing::warn!("{}: vet pass skipped: asking for suggestions: {e}", base.id);
            return;
        }
    };
    if waiting.is_empty() {
        return;
    }
    let batch = take_batch(&waiting);
    let ideas = match (Ideas {
        client: &base.client,
        repo_id: repo,
        token: Some(token),
    })
    .list()
    .await
    {
        Ok(ideas) => ideas,
        Err(e) => {
            tracing::warn!("{}: vet pass skipped: reading the ideas: {e}", base.id);
            return;
        }
    };
    let cwd = Path::new(&base.project.repo_path);
    let specs_index = std::fs::read_to_string(cwd.join("docs").join("specs").join("README.md")).unwrap_or_default();
    let past = state.lock().unwrap().recent.clone();
    let vetter = Vetter {
        template: TEMPLATE,
        project_type: base.project.project_type,
        specs_index: &specs_index,
        ideas: &ideas,
        past: &past,
        session: EphemeralSessionConfig {
            cwd,
            budget_usd: base.project.budget.scout_usd,
            model: "haiku".to_string(),
            local: None,
            engine: Some(placed.as_ref().map_or(EngineId::Claude, |p| p.engine)),
            cursor: placed
                .as_ref()
                .and_then(|p| p.cursor.clone())
                .map(|c| with_project(&base.project, c)),
            sandbox: sandbox_enabled(base.project.sandbox, cfg!(windows)),
            shell: false,
        },
    };
    let planned = plan(&vetter, batch, runner).await;
    let Some(verdicts) = planned.verdicts else {
        tracing::warn!(
            "{}: vet pass sent nothing: {}",
            base.id,
            planned.note.unwrap_or_default()
        );
        return;
    };
    for j in &verdicts {
        send(base, repo, token, j).await;
    }
    let mut st = state.lock().unwrap();
    let mut counts = BTreeMap::new();
    for j in &verdicts {
        *counts.entry(name(j.verdict).to_string()).or_insert(0) += 1;
        st.recent.push(PastVerdict {
            verdict: j.verdict,
            reason: j.reason.clone(),
        });
    }
    let over = st.recent.len().saturating_sub(PAST_KEPT);
    st.recent.drain(..over);
    st.last = Some(Last {
        day: day.to_string(),
        counts,
    });
    st.dirty = true;
}

async fn send(base: &PlaytestJob, repo: u64, token: &str, j: &Judged) {
    let v = VerdictSend {
        id: j.id.clone(),
        verdict: j.verdict,
        reason: j.reason.clone(),
        matches: j.matches.clone(),
    };
    match base.client.suggestion_verdict(repo, Some(token), &v).await {
        Ok(VerdictSent::Sent) => {}
        Ok(VerdictSent::AlreadyAnswered) => {
            tracing::info!("{}: suggestion {} was already answered", base.id, j.id)
        }
        Err(e) => tracing::warn!("{}: sending the verdict for {} failed: {e}", base.id, j.id),
    }
}
