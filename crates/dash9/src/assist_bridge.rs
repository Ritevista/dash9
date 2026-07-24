//! Wires the already-built `dash9-assist` crate (PR7) into a live
//! `dash9 open` session (`docs/specs/assist.md` Section A explicitly
//! deferred this: "a thin wiring exercise... not a redesign"). Whole
//! file is `#[cfg(feature = "assist")]`-only via its declaration in
//! `main.rs` — never compiled, and never linked against
//! `dash9-assist`, when that optional feature is off.
//!
//! `crate::live_session` stays entirely unaware this file exists —
//! everything assist-specific lives here and in `open.rs`'s
//! `run_with_assist`, so the non-assist path (`open::run_plain`,
//! `live_session.rs`) carries zero risk from this integration.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dash9_assist::{
    ActiveDatasourceMetadata, AssistConfig, AssistContext, AssistOutcome, AssistSession,
    DatasourceSummary, HttpLlmClient, ProposedCommand, TimeRangeSummary,
};
use dash9_core::{Command, CommandSource, Datasource, LogLine, SessionLogEntry};
use dash9_prom::PrometheusDatasource;
use tokio::sync::{mpsc, Mutex};

use crate::datasources::epoch_ms_now;
use crate::live_session::{execute_command, LiveSession};

/// `~/.config/dash9/assist.toml` (`docs/specs/assist.md` Section D).
/// No `dirs`/`home` crate in this workspace — `HOME` is enough for
/// this project's target environment.
fn default_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".config/dash9/assist.toml"))
}

/// Loads the assist config and builds a session, or returns the
/// reason it couldn't — never fatal to the caller (mirrors `dash
/// open`'s failure handling: a broken/absent assist config must not
/// kill the whole `dash9 open` session).
pub fn load_assist_session(
    workspace_root: PathBuf,
) -> Result<Arc<Mutex<AssistSession<HttpLlmClient>>>, String> {
    let Some(path) = default_config_path() else {
        return Err("$HOME is not set; cannot locate ~/.config/dash9/assist.toml".to_string());
    };
    let config = AssistConfig::load(&path).map_err(|err| err.to_string())?;
    let client = HttpLlmClient::new(config.clone());
    let session = AssistSession::new(client, &config, workspace_root);
    Ok(Arc::new(Mutex::new(session)))
}

/// The parts of `AssistContext` buildable synchronously with no
/// network access. Computed in the render loop right before spawning
/// `spawn_ask`, not inside the spawned task, so that task never needs
/// a `LiveSession` reference — it only ever receives owned data.
pub fn static_context_parts(
    session: &LiveSession,
    now_ms: i64,
) -> (Vec<DatasourceSummary>, Option<String>, TimeRangeSummary) {
    let mut names: Vec<&String> = session.datasources.keys().collect();
    names.sort();
    let datasources = names
        .into_iter()
        .map(|name| DatasourceSummary {
            name: name.clone(),
            datasource_type: session.datasources[name].datasource_type.to_string(),
        })
        .collect();

    let dashboard_toml = session.to_toml_string();
    let range_ms = session.range().as_millis();
    let time_range = TimeRangeSummary {
        start_ms: now_ms - range_ms,
        end_ms: now_ms,
    };

    (datasources, dashboard_toml, time_range)
}

/// Spawns the background `ask()` call: fetches the focused
/// datasource's metric/label metadata (a fetch failure degrades to
/// empty lists rather than failing the whole ask — the assistant just
/// gets less context, not an error), assembles the full
/// `AssistContext`, runs the contract loop, and sends the outcome
/// back. Never blocks the render loop — the `Mutex` is only ever
/// locked from inside this task, never from the render loop's own
/// thread.
pub fn spawn_ask(
    assist: Arc<Mutex<AssistSession<HttpLlmClient>>>,
    focused_datasource: Option<(String, Arc<PrometheusDatasource>)>,
    static_parts: (Vec<DatasourceSummary>, Option<String>, TimeRangeSummary),
    focused_panel: bool,
    request: String,
    update_tx: mpsc::Sender<AssistOutcome>,
) {
    tokio::spawn(async move {
        let (datasources, dashboard_toml, time_range) = static_parts;

        let active_datasource_metadata = match focused_datasource {
            Some((name, adapter)) => {
                let metric_names = adapter.metric_names().await.unwrap_or_default();
                let label_keys = adapter.label_names().await.unwrap_or_default();
                Some(ActiveDatasourceMetadata {
                    datasource_name: name,
                    metric_names,
                    label_keys,
                })
            }
            None => None,
        };

        let context = AssistContext {
            datasources,
            active_datasource_metadata,
            dashboard_toml,
            time_range,
        };

        let outcome = assist
            .lock()
            .await
            .ask(&context, &request, focused_panel)
            .await;
        let _ = update_tx.send(outcome).await;
    });
}

/// The AI-integration analogue of `live_session::execute_command`:
/// pure/sync dispatch of one delivered `AssistOutcome`. Every
/// `AutoRun` command runs immediately through the exact same
/// `execute_command` a human-typed command uses; every `Proposal` is
/// queued, never executed, until the caller applies or dismisses it
/// (`docs/specs/assist.md` Section H/I — no invisible assistant
/// action, a proposal is staged, not silently run).
pub fn handle_assist_outcome(
    session: &mut LiveSession,
    log: &mut Vec<LogLine>,
    pending: &mut VecDeque<Command>,
    focused_panel: usize,
    outcome: AssistOutcome,
) {
    match outcome {
        AssistOutcome::Turn(turn) => {
            if let Some(sentence) = turn.intent_sentence {
                log.push(LogLine::Result(sentence));
            }
            for proposed in turn.commands {
                let cmd = match &proposed {
                    ProposedCommand::AutoRun(cmd) | ProposedCommand::Proposal(cmd) => cmd.clone(),
                };
                log.push(LogLine::Command(SessionLogEntry {
                    source: CommandSource::Assistant,
                    command_text: format!("{cmd:?}"),
                    timestamp_ms: epoch_ms_now(),
                }));
                match proposed {
                    ProposedCommand::AutoRun(cmd) => {
                        let result = execute_command(session, focused_panel, cmd);
                        log.push(LogLine::Result(result));
                    }
                    ProposedCommand::Proposal(cmd) => {
                        pending.push_back(cmd);
                        log.push(LogLine::Result(
                            "proposal — press y to apply, n to dismiss".to_string(),
                        ));
                    }
                }
            }
        }
        AssistOutcome::Refusal(sentence) => {
            log.push(LogLine::Result(format!("assistant: {sentence}")));
        }
        AssistOutcome::Failed(err) => log.push(LogLine::Result(format!("assistant error: {err}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash9_assist::AssistTurn;
    use dash9_core::{
        DatasourceType, Duration, DurationUnit, GridSpec, PanelType, RefreshInterval,
        ValidatedDashboard, ValidatedDatasource, ValidatedPanel,
    };
    use tokio::sync::mpsc;

    fn sample_session() -> (LiveSession, tempfile::TempDir) {
        let dashboard = ValidatedDashboard {
            title: "Test".to_string(),
            refresh: RefreshInterval::Duration(Duration {
                magnitude: 30,
                unit: DurationUnit::Seconds,
            }),
            default_range: Duration {
                magnitude: 1,
                unit: DurationUnit::Hours,
            },
            test_latency_budget: Duration {
                magnitude: 5,
                unit: DurationUnit::Seconds,
            },
            datasources: vec![ValidatedDatasource {
                name: "prom".to_string(),
                datasource_type: DatasourceType::Prometheus,
                url: "http://127.0.0.1:1".to_string(),
            }],
            panels: vec![ValidatedPanel {
                title: "CPU".to_string(),
                panel_type: PanelType::Timeseries,
                datasource: "prom".to_string(),
                query: "up".to_string(),
                allow_empty: false,
                latency_budget: None,
                grid: GridSpec {
                    row: 0,
                    col: 0,
                    w: 1,
                    h: 1,
                },
                thresholds: vec![],
            }],
        };
        let workspace = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let session = LiveSession::new(&dashboard, workspace.path().to_path_buf(), tx);
        (session, workspace)
    }

    #[tokio::test]
    async fn static_context_parts_includes_datasources_toml_and_time_range() {
        let (session, _workspace) = sample_session();
        let (datasources, dashboard_toml, time_range) = static_context_parts(&session, 10_000);

        assert_eq!(datasources.len(), 1);
        assert_eq!(datasources[0].name, "prom");
        assert_eq!(datasources[0].datasource_type, "prometheus");

        let toml = dashboard_toml.expect("dashboard_toml should serialize");
        assert!(toml.contains("title = \"Test\""));

        assert_eq!(time_range.end_ms, 10_000);
        assert_eq!(time_range.start_ms, 10_000 - session.range().as_millis());
    }

    #[tokio::test]
    async fn autorun_command_executes_immediately_and_logs_twice() {
        let (mut session, _workspace) = sample_session();
        let mut log = Vec::new();
        let mut pending = VecDeque::new();

        let outcome = AssistOutcome::Turn(AssistTurn {
            intent_sentence: Some("Sure.".to_string()),
            commands: vec![ProposedCommand::AutoRun(Command::PanelTitle {
                text: "Renamed by AI".to_string(),
            })],
            raw_reply: String::new(),
            usage: None,
        });
        handle_assist_outcome(&mut session, &mut log, &mut pending, 0, outcome);

        assert_eq!(session.panels[0].title, "Renamed by AI");
        assert!(pending.is_empty());
        // intent sentence + command + result = 3 lines
        assert_eq!(log.len(), 3);
    }

    #[tokio::test]
    async fn proposal_command_is_queued_not_executed() {
        let (mut session, _workspace) = sample_session();
        let mut log = Vec::new();
        let mut pending = VecDeque::new();
        let original_title = session.panels[0].title.clone();

        let outcome = AssistOutcome::Turn(AssistTurn {
            intent_sentence: None,
            commands: vec![ProposedCommand::Proposal(Command::Refresh {
                interval: RefreshInterval::Off,
            })],
            raw_reply: String::new(),
            usage: None,
        });
        handle_assist_outcome(&mut session, &mut log, &mut pending, 0, outcome);

        assert_eq!(session.panels[0].title, original_title, "must not execute");
        assert_eq!(pending.len(), 1);
        assert_eq!(log.len(), 2); // command + "press y/n" result
    }

    #[tokio::test]
    async fn refusal_and_failure_each_log_one_line() {
        let (mut session, _workspace) = sample_session();
        let mut log = Vec::new();
        let mut pending = VecDeque::new();

        handle_assist_outcome(
            &mut session,
            &mut log,
            &mut pending,
            0,
            AssistOutcome::Refusal("no.".to_string()),
        );
        assert_eq!(log.len(), 1);

        handle_assist_outcome(
            &mut session,
            &mut log,
            &mut pending,
            0,
            AssistOutcome::Failed(dash9_assist::AssistError::Timeout),
        );
        assert_eq!(log.len(), 2);
    }
}
