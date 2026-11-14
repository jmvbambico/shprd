//! Agent CLIs as data: the harness table, CLI detection, sign-in, models, quota
//! probes, skills discovery. M9 grows this into the agent adapters, one per
//! program, still driven by `data/harness/*.toml`.

pub mod detect;
pub mod env;
pub mod families;
pub mod models;
pub mod quota;
#[cfg(test)]
mod scripted;
pub mod skills;
pub mod spec;
