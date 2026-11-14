//! Agent CLIs as data: the harness table, CLI detection, sign-in, models, quota
//! probes, skills discovery, and — since M9 — the per-program adapters that turn
//! a role's pick into a launch, still driven by `data/harness/*.toml`.

pub mod adapter;
pub mod asks;
pub mod detect;
pub mod env;
pub mod families;
pub mod harness;
pub mod models;
pub mod quota;
pub mod roles;
#[cfg(test)]
mod scripted;
pub mod skills;
pub mod spec;
