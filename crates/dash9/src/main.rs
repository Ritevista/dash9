//! `dash9` binary: `open`, `test`, `demo` subcommands. See `SPEC.md`.

#[cfg(feature = "assist")]
mod assist_bridge;
mod dashboard_loader;
mod datasources;
mod demo;
mod live_session;
mod log_recorder;
mod open;
mod selection;
mod test_cmd;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

/// SPEC.md C.2's own worked-example datasource URL — the default
/// every Prometheus-typed panel in an imported Grafana dashboard
/// resolves to when `--prometheus-url` isn't given (`docs/specs/
/// grafana-dashboards.md` Section D). A TOML dashboard ignores this
/// entirely; it declares its own `[[datasources]] url`.
const DEFAULT_PROMETHEUS_URL: &str = "http://localhost:9090";

#[derive(Parser)]
#[command(name = "dash9", about = "A terminal UI for observability dashboards")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Open a dashboard file — TOML or Grafana JSON, detected from the
    /// file itself — in the interactive TUI.
    ///
    /// Format detection: `docs/specs/grafana-dashboards.md` Section F.
    /// Natural language input (backed by an OpenAI-compatible endpoint
    /// configured at `~/.config/dash9/assist.toml`) is available
    /// whenever the binary was built with the `assist` feature (on by
    /// default) — no separate flag needed; toggle it at runtime with
    /// `/ai on`/`/ai off` (`docs/specs/open.md` Section D).
    Open {
        /// Dashboard file to open — TOML or Grafana JSON, detected from
        /// the file itself. Required: dash9 has no "start empty" mode
        /// yet, so a session always begins from a real file on disk. To
        /// build a dashboard up from nothing, hand-write a minimal TOML
        /// file first (a `[dashboard]` header with an empty
        /// `panels = []` is enough), open that, then add datasources
        /// live with `/ds add` — adding new *panels* from inside the
        /// TUI isn't supported yet; edit the file and `/dash open` it
        /// again.
        path: PathBuf,
        /// Prometheus URL every Prometheus-typed panel resolves to
        /// when opening a Grafana JSON dashboard (which carries only
        /// an internal datasource uid, never a queryable URL).
        /// Ignored for a TOML dashboard, which declares its own.
        #[arg(long, default_value = DEFAULT_PROMETHEUS_URL)]
        prometheus_url: String,
    },
    /// Validate a dashboard file headlessly (SPEC.md Section C.3) —
    /// TOML or Grafana JSON, same detection as `open`.
    Test {
        /// Dashboard file to validate. See `open`'s PATH for format
        /// detection; always required, same as `open`.
        path: PathBuf,
        /// See `open --prometheus-url`.
        #[arg(long, default_value = DEFAULT_PROMETHEUS_URL)]
        prometheus_url: String,
    },
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
        Commands::Open {
            path,
            prometheus_url,
        } => open::run(&path, &prometheus_url),
        Commands::Test {
            path,
            prometheus_url,
        } => {
            let code = test_cmd::run(&path, &prometheus_url).await?;
            std::process::exit(code);
        }
        Commands::Demo { assist } => demo::run(assist),
    }
}
