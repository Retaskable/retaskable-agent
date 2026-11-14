//! What the deploy request loop touches outside itself: the store and the code that runs a request. A trait so the
//! tests use fakes; [`super::Live`] is the real one.
use std::future::Future;

use serde_json::{Map, Value};

use crate::backlog::deploy::{DeployRequest, Finish};

/// How a request ran to its end: `Done`, or `Failed` for one that ran and didn't succeed (a TestFlight build that
/// didn't end `submitted`), with the derived result only.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub status: Finish,
    pub result: Map<String, Value>,
}

/// Why a request didn't run to its end.
#[derive(Debug, Clone, PartialEq)]
pub enum RunError {
    /// The code's own refusal ("Name a commit."): `refused`, with this as the reason.
    Refused(String),
    /// Anything else: `failed`, with this as the reason.
    Failed(String),
}

pub trait Deps: Send + Sync {
    /// This computer's name, as a claim records it and as a request's `prefer` names it.
    fn machine(&self) -> String;
    /// The time now, in milliseconds since the epoch.
    fn now_ms(&self) -> u64;
    /// `deploy_list`.
    fn list(&self) -> impl Future<Output = Result<Vec<DeployRequest>, String>> + Send;
    /// Whether this computer holds what the request needs (its keys, sign-ins, setup, and is not paused where that
    /// matters): checked here, never by the server. A request this says no to is left to another computer.
    fn can(&self, r: &DeployRequest) -> impl Future<Output = bool> + Send;
    /// `deploy_claim`: whether this computer won it.
    fn claim(&self, id: &str) -> impl Future<Output = Result<bool, String>> + Send;
    /// `deploy_beat`: whether this computer still holds the claim.
    fn beat(&self, id: &str) -> impl Future<Output = Result<bool, String>> + Send;
    /// `deploy_finish`: whether the store took the report.
    fn finish(
        &self,
        id: &str,
        status: Finish,
        result: Option<Map<String, Value>>,
    ) -> impl Future<Output = Result<bool, String>> + Send;
    /// Runs it, with the code the website's pushed command ran.
    fn run(&self, r: &DeployRequest) -> impl Future<Output = Result<Outcome, RunError>> + Send;
}
