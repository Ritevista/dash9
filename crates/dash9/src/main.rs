//! `dash9` binary: `open`, `test`, `demo` subcommands. See `SPEC.md`.

#[cfg(feature = "assist")]
mod assist_bridge;
mod datasources;
mod demo;
mod live_session;
mod log_recorder;
mod open;
mod selection;
mod test_cmd;

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
    Open {
        path: PathBuf,
        /// Enable natural-language input alongside the command
        /// grammar, backed by an OpenAI-compatible endpoint configured
        /// at `~/.config/dash9/assist.toml`. Requires the `assist`
        /// feature (on by default).
        #[arg(long)]
        assist: bool,
    },
    /// Validate a dashboard TOML file headlessly (SPEC.md Section C.3).
    Test { path: PathBuf },
    /// Run a self-contained demo panel against synthetic data.
    Demo {
        /// Also run a scripted assistant walkthrough against canned
        /// fixtures — no network (docs/specs/assist.md Section K).
        /// Requires the `assist` feature (on by default).
        #[arg(long)]
        assist: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Open { path, assist } => open::run(&path, assist),
        Commands::Test { path } => {
            let code = test_cmd::run(&path).await?;
            std::process::exit(code);
        }
        Commands::Demo { assist } => demo::run(assist),
    }
}
