//! Projectors: dagr run.json, brief, status, PR body (pure functions of events).
//!
//! Every view here is a pure function of [`shprd_core::state::RunState`] and,
//! where the log holds facts the state drops, of the event log itself. None of
//! them calls a model: status is rendered from the log, never written by an
//! agent.

pub mod brief;
mod clock;
pub mod dagr;
pub mod pr;
pub mod status;

#[cfg(test)]
mod fixture;
