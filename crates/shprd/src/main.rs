//! The binary: clap CLI, wiring.

use clap::Parser;

#[derive(Parser)]
#[command(name = "shprd", version)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
