//! Events, fold, task model, transition rules, config (no I/O beyond the log
//! file), and the spawn retry every external program shprd runs goes through.

pub mod ask;
pub mod config;
pub mod event;
pub mod gate;
pub mod hash;
pub mod log;
pub mod prompts;
pub mod review;
pub mod rules;
pub mod scope;
pub mod settings;
pub mod spawn;
pub mod state;
pub mod tick;
