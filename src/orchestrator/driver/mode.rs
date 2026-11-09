//! The orchestrator mode switch (`RETASKABLE_ORCHESTRATOR`).
use crate::env::Env;

/// The variable that switches the orchestrator on.
pub const MODE_VAR: &str = "RETASKABLE_ORCHESTRATOR";

/// What the orchestrator may do. Off by default, so installing a build never starts it by surprise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OrchestratorMode {
    /// Nothing runs.
    #[default]
    Off,
    /// Reads the backlog and writes `tick` events saying what a tick would do. Takes no action.
    Dry,
    /// Claims work and applies starts, but only with a [`SessionStarter`] and while the port guard
    /// (`guard.rs`) sees no other agent on this computer. With no starter it behaves exactly like `Dry`.
    Claim,
    /// Takes work as short runs on pieces (pieces.md section 3), with the same port guard as `claim`. The kinds
    /// it can take are the [`RunStarter`](super::RunStarter)s it is given; with none it drops old runs and asks for nothing.
    Runs,
}

impl OrchestratorMode {
    /// The mode `RETASKABLE_ORCHESTRATOR` names (`off`, `dry`, `claim` or `runs`, any case), and a warning
    /// for a value that is none of them, which counts as `off`.
    pub fn from_env(env: &Env) -> (Self, Option<String>) {
        let Some(raw) = env.get(MODE_VAR) else {
            return (Self::Off, None);
        };
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => (Self::Off, None),
            "dry" => (Self::Dry, None),
            "claim" => (Self::Claim, None),
            "runs" => (Self::Runs, None),
            other => (
                Self::Off,
                Some(format!(
                    "{MODE_VAR}={other:?} isn't off, dry, claim or runs, so the orchestrator stays off."
                )),
            ),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Dry => "dry",
            Self::Claim => "claim",
            Self::Runs => "runs",
        }
    }

    /// Reads the mode and logs it once, with the warning or the note that `claim` isn't built.
    pub fn read_and_log(env: &Env) -> Self {
        let (mode, warning) = Self::from_env(env);
        if let Some(w) = warning {
            tracing::warn!("{w}");
        }
        match mode {
            Self::Off => {
                tracing::info!("orchestrator: off (set {MODE_VAR}=dry to read the backlog and log what it would do)")
            }
            Self::Dry => {
                tracing::info!("orchestrator: dry (reads the backlog, logs tick events, never claims or starts work)")
            }
            Self::Claim => tracing::warn!(
                "orchestrator: claim (claims work only once a session runner is given; until then it runs dry)"
            ),
            Self::Runs => tracing::info!(
                "orchestrator: runs (takes work as short runs of the kinds it can run; with none it only drops old runs)"
            ),
        }
        mode
    }
}
