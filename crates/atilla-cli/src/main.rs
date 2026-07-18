//! The atilla CLI. A thin shell: parse argv, hand off to atilla-core.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "atilla", version, about = "atilla")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the engine. Placeholder until the real surface lands.
    Run,
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().cmd {
        Cmd::Run => {
            println!("{}", atilla_core::run()?);
            Ok(())
        }
    }
}
