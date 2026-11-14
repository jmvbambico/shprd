//! Events, fold, task model, transition rules, config (no I/O beyond the log
//! file), and the spawn retry every external program shprd runs goes through.

pub mod config;
pub mod event;
pub mod gate;
pub mod hash;
pub mod log;
pub mod rules;
pub mod scope;
pub mod spawn;
pub mod state;
pub mod tick;
