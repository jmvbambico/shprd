//! Harness table, CLI detection, sign-in, models, quota probes, skills discovery.

pub mod detect;
pub mod env;
pub mod families;
pub mod models;
pub mod quota;
#[cfg(test)]
mod scripted;
pub mod skills;
pub mod spec;
