//! Agent CLIs as data: the harness table, CLI detection, sign-in, models, quota
//! probes, skills discovery, and — since M9 — the per-program adapters that turn
//! a role's pick into a launch, still driven by `data/harness/*.toml`.
//!
//! The data itself is found through [`data::DataDir`]: `$SHPRD_DATA_DIR` when a
//! person names one, else their overrides under the data home, else the copy
//! embedded in the binary, so an installed `shprd` needs no data directory.

pub mod adapter;
pub mod asks;
pub mod data;
pub mod detect;
pub mod env;
pub mod exhausted;
pub mod families;
pub mod harness;
pub mod models;
pub mod quota;
pub mod roles;
#[cfg(test)]
mod scripted;
pub mod skills;
pub mod spec;
