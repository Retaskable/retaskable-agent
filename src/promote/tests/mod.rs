//! Promote against real temp repos with a bare origin, a fake `gh` and a fake staging server.
#[cfg(unix)]
pub(crate) mod fixture;
mod log;
mod pure;
#[cfg(unix)]
mod run;
#[cfg(unix)]
mod site;
