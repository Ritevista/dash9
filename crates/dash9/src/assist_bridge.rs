//! Wires the already-built `dash9-assist` crate (PR7) into a live
//! `dash9 open` session (`docs/specs/assist.md` Section A explicitly
//! deferred this: "a thin wiring exercise... not a redesign"). Whole
//! file is `#[cfg(feature = "assist")]`-only via its declaration in
//! `main.rs` — never compiled, and never linked against
//! `dash9-assist`, when that optional feature is off.
//!
//! [`AssistHandler`] implements `dash9_tui::shell::CommandHandler` —
//! the render loop (`open::shell_loop`) doesn't know or care that this
//! handler talks to an LLM; it's just another implementation of the
//! same trait `GrammarOnlyHandler` (`open.rs`) implements. `crate::
//! live_session` itself stays entirely unaware this file exists.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dash9_assist::{
    ActiveDatasourceMetadata, AssistConfig, AssistContext, AssistOutcome, AssistSession,
    DatasourceSummary, HttpLlmClient, ProposedCommand, TimeRangeSummary,
};
use dash9_core::{Command, CommandSource, Datasource, LogLine, SessionLogEntry};
use dash9_prom::PrometheusDatasource;
use dash9_tui::shell::{CommandHandler, CommandResponse, ShellInput};
use dash9_tui::{help_text, AssistStatusLine, StatusBarModel};
use tokio::sync::{mpsc, Mutex};

use crate::datasources::epoch_ms_now;
use crate::live_session::{execute_command, LiveSession};
use crate::log_recorder::LogRecorder;
use crate::open::{status_bar_for, HasSession};

const CHANNEL_CAPACITY: usize = 64;

/// `~/.config/dash9/assist.toml` (`docs/specs/assist.md` Section D).
/// No `dirs`/`home` crate in this workspace — `HOME` is enough for
/// this project's target environment.
fn default_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| Path::new(&home).join(".config/dash9/assist.toml"))
}

/// Loads the assist config and builds a session, or returns the
/// reason it couldn't — never fatal to the caller (mirrors `dash
/// open`'s failure handling: a broken/absent assist config must not
/// kill the whole `dash9 open` session). Returns the loaded
/// `AssistConfig` alongside the session handle because model
/// switching needs to clone-and-override it later.
type LoadedAssistSession = (Arc<Mutex<AssistSession<HttpLlmClient>>>, AssistConfig);

fn load_assist_session(workspace_root: PathBuf) -> Result<LoadedAssistSession, String> {
    let Some(path) = default_config_path() else {
        return Err("$HOME is not set; cannot locate ~/.config/dash9/assist.toml".to_string());
    };
    let config = AssistConfig::load(&path).map_err(|err| err.to_string())?;
    let client = HttpLlmClient::new(config.clone());
    let session = AssistSession::new(client, &config, workspace_root);
    Ok((Arc::new(Mutex::new(session)), config))
}

/// The parts of `AssistContext` buildable synchronously with no
/// network access. Computed in `execute()` right before spawning
/// `spawn_ask`, not inside the spawned task, so that task never needs
/// a `LiveSession` reference — it only ever receives owned data.
fn static_context_parts(
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
fn spawn_ask(
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

/// Everything specific to an available, loaded assist config —
/// `AssistHandler::assist` is `None` when the config couldn't load,
/// so every method that needs this never has to handle "loaded but
/// broken," only "loaded" vs "not available."
struct AssistCore {
    handle: Arc<Mutex<AssistSession<HttpLlmClient>>>,
    config: AssistConfig,
    workspace_root: PathBuf,
    update_tx: mpsc::Sender<AssistOutcome>,
    update_rx: mpsc::Receiver<AssistOutcome>,
    enabled: bool,
    model: String,
    /// Mirrors `AssistStatusModel`'s connectivity, tracked locally
    /// rather than read through `handle`'s `Mutex` each render tick —
    /// that would need an `.await`, which the render loop's plain
    /// sync closure can't do. Set to `"waiting"` the instant a request
    /// is submitted, updated when the outcome arrives.
    connectivity: String,
    tokens_sent: u32,
    tokens_received: u32,
}

pub struct AssistHandler {
    session: LiveSession,
    update_rx: mpsc::Receiver<crate::live_session::SessionUpdate>,
    assist: Option<AssistCore>,
    recorder: Arc<std::sync::Mutex<LogRecorder>>,
}

impl AssistHandler {
    /// Returns the handler plus an optional startup message (e.g. why
    /// assist isn't available) for the caller to log before entering
    /// the render loop. `recorder` is the same handle `open::shell_loop`
    /// drains every tick — recording is handler-agnostic, not an
    /// assist-only concern (see `log_recorder`'s module docs).
    pub fn new(
        session: LiveSession,
        update_rx: mpsc::Receiver<crate::live_session::SessionUpdate>,
        workspace_root: PathBuf,
        recorder: Arc<std::sync::Mutex<LogRecorder>>,
    ) -> (Self, Option<String>) {
        match load_assist_session(workspace_root.clone()) {
            Ok((handle, config)) => {
                let model = config.model.clone();
                let (update_tx, assist_rx) = mpsc::channel(CHANNEL_CAPACITY);
                let core = AssistCore {
                    handle,
                    config,
                    workspace_root,
                    update_tx,
                    update_rx: assist_rx,
                    enabled: true,
                    model,
                    connectivity: "idle".to_string(),
                    tokens_sent: 0,
                    tokens_received: 0,
                };
                (
                    Self {
                        session,
                        update_rx,
                        assist: Some(core),
                        recorder,
                    },
                    None,
                )
            }
            Err(reason) => (
                Self {
                    session,
                    update_rx,
                    assist: None,
                    recorder,
                },
                Some(format!("assist unavailable: {reason}")),
            ),
        }
    }
}

impl HasSession for AssistHandler {
    fn session(&self) -> &LiveSession {
        &self.session
    }
}

impl CommandHandler for AssistHandler {
    fn execute(&mut self, input: ShellInput, focused_panel: usize) -> CommandResponse {
        match input {
            ShellInput::Grammar(Command::Quit) => CommandResponse {
                should_quit: true,
                ..CommandResponse::default()
            },
            ShellInput::Grammar(cmd) => {
                CommandResponse::result(execute_command(&mut self.session, focused_panel, cmd))
            }
            ShellInput::Help(topic) => CommandResponse::result(help_text(topic.as_deref())),
            ShellInput::CommandError(err) => CommandResponse::result(err.to_string()),
            ShellInput::Export { format, path } => CommandResponse::result(
                self.session
                    .export_panel(focused_panel, format, path.as_deref()),
            ),
            ShellInput::RecordingStatus => CommandResponse::result(
                self.recorder
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .status(),
            ),
            ShellInput::SetRecording { on, path } => CommandResponse::result(
                self.recorder
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .set(on, path),
            ),
            ShellInput::ModelStatus => CommandResponse::result(self.model_status()),
            ShellInput::ModelSwitch(name) => CommandResponse::result(self.switch_model(&name)),
            ShellInput::AssistStatus => CommandResponse::result(self.assist_status()),
            ShellInput::SetAssist(on) => CommandResponse::result(self.set_assist(on)),
            ShellInput::ToggleAssist => CommandResponse::result(self.toggle_assist()),
            ShellInput::Shell(command) => {
                CommandResponse::result(self.session.spawn_shell_command(&command))
            }
            ShellInput::NaturalLanguage(text) => self.ask_or_fallback(focused_panel, text),
        }
    }

    fn poll(&mut self, focused_panel: usize) -> Option<CommandResponse> {
        if let Ok(update) = self.update_rx.try_recv() {
            let mut log_entries = Vec::new();
            self.session.apply_update(update, &mut log_entries);
            return Some(CommandResponse {
                log_entries,
                ..CommandResponse::default()
            });
        }
        let core = self.assist.as_mut()?;
        let Ok(outcome) = core.update_rx.try_recv() else {
            return None;
        };
        core.connectivity = match &outcome {
            AssistOutcome::Failed(err) => format!("error: {err}"),
            _ => "idle".to_string(),
        };
        if let AssistOutcome::Turn(turn) = &outcome {
            if let Some(usage) = turn.usage {
                core.tokens_sent += usage.prompt_tokens;
                core.tokens_received += usage.completion_tokens;
            }
        }
        Some(handle_assist_outcome(
            &mut self.session,
            focused_panel,
            outcome,
        ))
    }

    fn panel_count(&self) -> usize {
        self.session.panels.len()
    }

    fn status_bar(&self) -> StatusBarModel {
        let assist = self.assist.as_ref().map(|core| AssistStatusLine {
            model: core.model.clone(),
            enabled: core.enabled,
            connectivity: core.connectivity.clone(),
            tokens_sent: core.tokens_sent,
            tokens_received: core.tokens_received,
        });
        status_bar_for(&self.session, assist)
    }
}

impl AssistHandler {
    fn model_status(&self) -> String {
        let Some(core) = &self.assist else {
            return "assist unavailable".to_string();
        };
        if core.config.known_models.is_empty() {
            format!("current model: {}", core.model)
        } else {
            format!(
                "current model: {}\nknown models: {}",
                core.model,
                core.config.known_models.join(", ")
            )
        }
    }

    /// Rebuilds the `AssistSession` with the new model — conversation
    /// history intentionally resets (decided with the user: switching
    /// models is "end this session, start a new one," not mutating a
    /// live session's model in place, which `dash9-assist`'s v1
    /// non-goal of "no runtime model switching" already rules out).
    fn switch_model(&mut self, name: &str) -> String {
        let Some(core) = &mut self.assist else {
            return "assist unavailable".to_string();
        };
        let mut new_config = core.config.clone();
        new_config.model = name.to_string();
        let client = HttpLlmClient::new(new_config.clone());
        let session = AssistSession::new(client, &new_config, core.workspace_root.clone());
        core.handle = Arc::new(Mutex::new(session));
        core.config = new_config;
        core.model = name.to_string();
        core.connectivity = "idle".to_string();
        format!("switched to model \"{name}\" (conversation history reset)")
    }

    fn toggle_assist(&mut self) -> String {
        let Some(core) = &mut self.assist else {
            return "assist unavailable".to_string();
        };
        core.enabled = !core.enabled;
        format!(
            "assistant turned {}",
            if core.enabled { "on" } else { "off" }
        )
    }

    /// `/ai on` / `/ai off` — explicit and idempotent, unlike the
    /// bare `a` key's toggle: setting the state it's already in
    /// reports that rather than flipping it.
    fn set_assist(&mut self, on: bool) -> String {
        let Some(core) = &mut self.assist else {
            return "assist unavailable".to_string();
        };
        if core.enabled == on {
            return format!("assistant already {}", if on { "on" } else { "off" });
        }
        core.enabled = on;
        format!("assistant turned {}", if on { "on" } else { "off" })
    }

    /// Bare `/ai`: combined on/off + model status, for a single
    /// glance at everything `/ai on|off` and `/model` each show a
    /// slice of.
    fn assist_status(&self) -> String {
        let Some(core) = &self.assist else {
            return "assist unavailable".to_string();
        };
        format!(
            "assistant: {}\n{}",
            if core.enabled { "on" } else { "off" },
            self.model_status()
        )
    }

    fn ask_or_fallback(&mut self, focused_panel: usize, text: String) -> CommandResponse {
        let unavailable_message =
            "no AI available — enable with /ai on, or use / for commands (see /help)";
        let Some(core) = &mut self.assist else {
            return CommandResponse::result(unavailable_message);
        };
        if !core.enabled {
            return CommandResponse::result(unavailable_message);
        }

        let now_ms = epoch_ms_now();
        let static_parts = static_context_parts(&self.session, now_ms);
        let focused_datasource = self.session.panels.get(focused_panel).and_then(|p| {
            self.session
                .datasources
                .get(&p.datasource)
                .map(|ds| (p.datasource.clone(), Arc::clone(&ds.adapter)))
        });
        let focused_panel_bool = !self.session.panels.is_empty();

        core.connectivity = "waiting".to_string();
        spawn_ask(
            Arc::clone(&core.handle),
            focused_datasource,
            static_parts,
            focused_panel_bool,
            text,
            core.update_tx.clone(),
        );
        CommandResponse::result("asking assistant…".to_string())
    }
}

/// The AI-integration analogue of `live_session::execute_command`:
/// pure/sync dispatch of one delivered `AssistOutcome`. Every
/// `AutoRun` command runs immediately through the exact same
/// `execute_command` a human-typed command uses; every `Proposal` is
/// queued (via `CommandResponse::new_proposals`), never executed,
/// until the caller applies or dismisses it (`docs/specs/assist.md`
/// Section H/I — no invisible assistant action, a proposal is staged,
/// not silently run).
fn handle_assist_outcome(
    session: &mut LiveSession,
    focused_panel: usize,
    outcome: AssistOutcome,
) -> CommandResponse {
    let mut response = CommandResponse::default();
    match outcome {
        AssistOutcome::Turn(turn) => {
            if let Some(sentence) = turn.intent_sentence {
                response.log_entries.push(LogLine::Result(sentence));
            }
            for proposed in turn.commands {
                let cmd = match &proposed {
                    ProposedCommand::AutoRun(cmd) | ProposedCommand::Proposal(cmd) => cmd.clone(),
                };
                response.log_entries.push(LogLine::Command(SessionLogEntry {
                    source: CommandSource::Assistant,
                    command_text: format!("{cmd:?}"),
                    timestamp_ms: epoch_ms_now(),
                }));
                match proposed {
                    ProposedCommand::AutoRun(cmd) => {
                        let result = execute_command(session, focused_panel, cmd);
                        response.log_entries.push(LogLine::Result(result));
                    }
                    ProposedCommand::Proposal(cmd) => {
                        response.new_proposals.push(cmd);
                        response.log_entries.push(LogLine::Result(
                            "proposal — press y to apply, n to dismiss".to_string(),
                        ));
                    }
                }
            }
        }
        AssistOutcome::Refusal(sentence) => {
            response
                .log_entries
                .push(LogLine::Result(format!("assistant: {sentence}")));
        }
        AssistOutcome::Failed(err) => {
            response
                .log_entries
                .push(LogLine::Result(format!("assistant error: {err}")));
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash9_assist::AssistTurn;
    use dash9_core::{
        DatasourceType, Duration, DurationUnit, GridSpec, PanelType, RefreshInterval,
        ValidatedDashboard, ValidatedDatasource, ValidatedPanel,
    };

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

        let outcome = AssistOutcome::Turn(AssistTurn {
            intent_sentence: Some("Sure.".to_string()),
            commands: vec![ProposedCommand::AutoRun(Command::PanelTitle {
                text: "Renamed by AI".to_string(),
            })],
            raw_reply: String::new(),
            usage: None,
        });
        let response = handle_assist_outcome(&mut session, 0, outcome);

        assert_eq!(session.panels[0].title, "Renamed by AI");
        assert!(response.new_proposals.is_empty());
        // intent sentence + command + result = 3 lines
        assert_eq!(response.log_entries.len(), 3);
    }

    #[tokio::test]
    async fn proposal_command_is_queued_not_executed() {
        let (mut session, _workspace) = sample_session();
        let original_title = session.panels[0].title.clone();

        let outcome = AssistOutcome::Turn(AssistTurn {
            intent_sentence: None,
            commands: vec![ProposedCommand::Proposal(Command::Refresh {
                interval: RefreshInterval::Off,
            })],
            raw_reply: String::new(),
            usage: None,
        });
        let response = handle_assist_outcome(&mut session, 0, outcome);

        assert_eq!(session.panels[0].title, original_title, "must not execute");
        assert_eq!(response.new_proposals.len(), 1);
        assert_eq!(response.log_entries.len(), 2); // command + "press y/n" result
    }

    #[tokio::test]
    async fn refusal_and_failure_each_log_one_line() {
        let (mut session, _workspace) = sample_session();

        let response =
            handle_assist_outcome(&mut session, 0, AssistOutcome::Refusal("no.".to_string()));
        assert_eq!(response.log_entries.len(), 1);

        let response = handle_assist_outcome(
            &mut session,
            0,
            AssistOutcome::Failed(dash9_assist::AssistError::Timeout),
        );
        assert_eq!(response.log_entries.len(), 1);
    }
}
