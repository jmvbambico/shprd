//! The binary: clap CLI, wiring, exit codes.
//!
//! Every verb reads the run log, checks the transition rules and appends one
//! event. A refusal writes nothing and exits 2; any other failure exits 1.

mod cli;
mod cmd;
mod ctx;
mod summary;

use std::process::ExitCode;

use clap::Parser;

use crate::cli::Cli;
use crate::ctx::CliError;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cmd::dispatch(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            report(&err);
            ExitCode::from(err.exit_code())
        }
    }
}

/// Writes a failure to stderr in the shape the task contract asks for.
fn report(err: &CliError) {
    match err {
        CliError::Refused(rule) => eprintln!("refused: {rule}"),
        CliError::Other(source) => eprintln!("error: {source:#}"),
    }
}
