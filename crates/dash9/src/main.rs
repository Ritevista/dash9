//! `dash9` binary: `open`, `test`, `demo` subcommands. See `SPEC.md`.

mod demo;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dash9", about = "A terminal UI for observability dashboards")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Open a dashboard TOML file in the interactive TUI.
    Open { path: PathBuf },
    /// Validate a dashboard TOML file headlessly (SPEC.md Section C.3).
    Test { path: PathBuf },
    /// Run a self-contained demo panel against synthetic data.
    Demo,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        // `open` and `test` need the dash9-prom datasource adapter,
        // which is not implemented yet; wiring them up is next.
        Commands::Open { path } => {
            println!("dash9 open {}: not yet implemented", path.display());
            Ok(())
        }
        Commands::Test { path } => {
            println!("dash9 test {}: not yet implemented", path.display());
            Ok(())
        }
        Commands::Demo => demo::run(),
    }
}
