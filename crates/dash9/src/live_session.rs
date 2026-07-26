//! Live, mutable dashboard session state for `dash9 open`'s command
//! bar. Replaces PR8's immutable `Arc<ValidatedDashboard>` model —
//! every SPEC.md Section B verb that mutates session state (`ds add`,
//! `panel type`/`threshold`/`title`, `range`, `refresh`, `dash save`,
//! `dash open`) needs somewhere real to apply to.
//!
//! `LiveSession` owns one polling task per panel (`tokio::spawn`, same
//! shape PR8 introduced). `range`/`refresh`/a panel's `panel_type` are
//! the only grammar-mutable fields that change what a poller
//! *fetches* — `query`/`datasource`/`grid` never change at runtime —
//! so those three live behind one `tokio::sync::watch` channel every
//! poller reads fresh each loop iteration, instead of needing its task
//! aborted and respawned on every edit. Only `dash open` (replaces the
//! entire session) tears down and rebuilds every poller.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use dash9_core::{
    load_path, validate, validate_workspace_relative_path, Command, CommandError, DashboardFile,
    DashboardMeta, Datasource, DatasourceSpec, DatasourceType, Duration, ErrorCode, Frame,
    GridSpec, LogLine, PanelSpec, PanelType, RefreshInterval, ThresholdSpec, ValidatedDashboard,
    ValidatedThreshold,
};
use dash9_prom::PrometheusDatasource;
use dash9_tui::{table_for_export, table_to_csv, table_to_markdown, ExportFormat};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

use crate::datasources::{epoch_ms_now, execute_panel_query};

/// One panel-poller result or one ad-hoc `q` result, delivered to the
/// render loop over one shared channel.
pub enum SessionUpdate {
    Panel {
        generation: u64,
        panel_index: usize,
        result: Result<Frame, CommandError>,
    },
    AdHoc {
        query: String,
        result: Result<Frame, CommandError>,
    },
    DsMetrics {
        datasource: String,
        result: Result<Vec<String>, CommandError>,
    },
    Shell {
        command: String,
        result: Result<ShellOutput, String>,
    },
}

/// The result of one `!<command>` run to completion
/// (`docs/specs/open.md` Section K). `status` is `None` only on a
/// signal-terminated process (Unix) — `std::process::ExitStatus::code()`
/// itself already returns `None` in that case, this just carries it
/// through rather than inventing a fake code.
pub struct ShellOutput {
    pub status: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

pub struct LiveDatasource {
    pub datasource_type: DatasourceType,
    pub url: String,
    pub adapter: Arc<PrometheusDatasource>,
}

pub struct LivePanel {
    pub title: String,
    pub panel_type: PanelType,
    pub datasource: String,
    pub query: String,
    pub allow_empty: bool,
    pub latency_budget: Option<Duration>,
    pub grid: GridSpec,
    pub thresholds: Vec<ValidatedThreshold>,
    pub last_result: Option<Result<Frame, CommandError>>,
}

impl From<&dash9_core::ValidatedPanel> for LivePanel {
    fn from(p: &dash9_core::ValidatedPanel) -> Self {
        LivePanel {
            title: p.title.clone(),
            panel_type: p.panel_type,
            datasource: p.datasource.clone(),
            query: p.query.clone(),
            allow_empty: p.allow_empty,
            latency_budget: p.latency_budget,
            grid: p.grid,
            thresholds: p.thresholds.clone(),
            last_result: None,
        }
    }
}

/// The subset of session state a poller needs live, kept behind one
/// `watch` channel so an edit takes effect without a task
/// abort/respawn. See module docs.
#[derive(Clone)]
struct RuntimeConfig {
    range: Duration,
    refresh: RefreshInterval,
    panel_types: Vec<PanelType>, // index-aligned with `LiveSession::panels`
}

pub struct LiveSession {
    pub title: String,
    pub datasources: HashMap<String, LiveDatasource>,
    pub panels: Vec<LivePanel>,
    test_latency_budget: Duration,
    workspace_root: PathBuf,
    generation: u64,
    config_tx: watch::Sender<RuntimeConfig>,
    config_rx: watch::Receiver<RuntimeConfig>,
    poller_handles: Vec<JoinHandle<()>>,
    update_tx: mpsc::Sender<SessionUpdate>,
}

impl LiveSession {
    pub fn new(
        dashboard: &ValidatedDashboard,
        workspace_root: PathBuf,
        update_tx: mpsc::Sender<SessionUpdate>,
    ) -> Self {
        let (config_tx, config_rx) = watch::channel(runtime_config_for(dashboard));
        let mut session = LiveSession {
            title: dashboard.title.clone(),
            datasources: build_live_datasources(dashboard),
            panels: dashboard.panels.iter().map(LivePanel::from).collect(),
            test_latency_budget: dashboard.test_latency_budget,
            workspace_root,
            generation: 0,
            config_tx,
            config_rx,
            poller_handles: Vec::new(),
            update_tx,
        };
        session.spawn_all_pollers();
        session
    }

    /// Applies one delivered `SessionUpdate`, pushing a `LogLine` for
    /// an ad-hoc `q` result. A panel-poller update whose `generation`
    /// doesn't match the current one is a stale result from a poller
    /// that was aborted by a `dash open` since it started this fetch
    /// — silently dropped, not applied.
    pub fn apply_update(&mut self, update: SessionUpdate, log: &mut Vec<LogLine>) {
        match update {
            SessionUpdate::Panel {
                generation,
                panel_index,
                result,
            } => {
                if generation == self.generation {
                    if let Some(panel) = self.panels.get_mut(panel_index) {
                        panel.last_result = Some(result);
                    }
                }
            }
            SessionUpdate::AdHoc { query, result } => {
                let text = match result {
                    Ok(frame) => format_ad_hoc_result(&query, &frame),
                    Err(err) => format!("q {query}: {err}"),
                };
                log.push(LogLine::Result(text));
            }
            SessionUpdate::DsMetrics { datasource, result } => {
                log.push(LogLine::Result(format_ds_metrics_result(
                    &datasource,
                    result,
                )));
            }
            SessionUpdate::Shell { command, result } => {
                log.push(LogLine::Result(format_shell_result(&command, result)));
            }
        }
    }

    fn spawn_all_pollers(&mut self) {
        for (index, panel) in self.panels.iter().enumerate() {
            // `validate()` guarantees every panel's `datasource`
            // matches a configured entry; still defensive here since
            // a background-task spawner should never panic.
            let Some(ds) = self.datasources.get(&panel.datasource) else {
                continue;
            };
            let handle = spawn_poller(
                index,
                self.generation,
                panel.query.clone(),
                Arc::clone(&ds.adapter),
                self.config_rx.clone(),
                self.update_tx.clone(),
            );
            self.poller_handles.push(handle);
        }
    }

    pub fn range(&self) -> Duration {
        self.config_rx.borrow().range
    }

    /// e.g. `"2 datasources (loki, prom)"` — for the status bar.
    pub fn datasource_summary(&self) -> String {
        if self.datasources.is_empty() {
            return "no datasources".to_string();
        }
        let mut names: Vec<&String> = self.datasources.keys().collect();
        names.sort();
        format!(
            "{} datasource{} ({})",
            names.len(),
            if names.len() == 1 { "" } else { "s" },
            names.into_iter().cloned().collect::<Vec<_>>().join(", ")
        )
    }

    /// Derived from whether any panel's last query result was an
    /// error — not a separate live probe (see
    /// `dash9_tui::DatasourceHealth`'s docs). `Unknown` until at least
    /// one panel has reported a result.
    pub fn datasource_health(&self) -> dash9_tui::DatasourceHealth {
        use dash9_tui::DatasourceHealth;

        let mut any_ok = false;
        let mut any_err = false;
        for panel in &self.panels {
            match &panel.last_result {
                Some(Ok(_)) => any_ok = true,
                Some(Err(_)) => any_err = true,
                None => {}
            }
        }
        if any_err {
            DatasourceHealth::Degraded
        } else if any_ok {
            DatasourceHealth::Healthy
        } else {
            DatasourceHealth::Unknown
        }
    }

    fn refresh(&self) -> RefreshInterval {
        self.config_rx.borrow().refresh
    }

    fn set_range(&mut self, duration: Duration) {
        self.config_tx.send_modify(|cfg| cfg.range = duration);
    }

    fn set_refresh(&mut self, interval: RefreshInterval) {
        self.config_tx.send_modify(|cfg| cfg.refresh = interval);
    }

    fn set_panel_type(&mut self, index: usize, panel_type: PanelType) {
        self.config_tx
            .send_modify(|cfg| cfg.panel_types[index] = panel_type);
    }

    /// "The focused datasource" (SPEC.md B.3's `q` description) is
    /// read as the focused panel's configured datasource — SPEC.md
    /// has no `ds focus` verb, so panel focus is the only grounded
    /// notion of "current" datasource an interactive session has.
    fn spawn_ad_hoc_query(&self, focused_panel: usize, query: &str) -> String {
        let Some(panel) = self.panels.get(focused_panel) else {
            return no_panel_focused_error();
        };
        let Some(datasource) = self.datasources.get(&panel.datasource) else {
            return format!(
                "{}: unknown datasource \"{}\"",
                ErrorCode::E101,
                panel.datasource
            );
        };
        let adapter = Arc::clone(&datasource.adapter);
        let update_tx = self.update_tx.clone();
        let query_for_task = query.to_string();
        tokio::spawn(async move {
            let now_ms = epoch_ms_now();
            let result = adapter
                .query(&query_for_task, now_ms)
                .await
                .map_err(|err| CommandError::new(ErrorCode::E106, err.to_string(), None));
            let _ = update_tx
                .send(SessionUpdate::AdHoc {
                    query: query_for_task,
                    result,
                })
                .await;
        });
        format!("q {query}: running…")
    }

    /// `ds metrics [name]`. `name` is looked up directly in
    /// `self.datasources`; omitted, it falls back to the focused
    /// panel's datasource — the same "focused datasource" convention
    /// `spawn_ad_hoc_query` uses, since SPEC.md has no `ds focus`
    /// verb. An unresolvable name (typo, or the assistant proposing
    /// one that was never added) is `E101`, exactly like an unknown
    /// datasource reached any other way.
    fn spawn_ds_metrics_query(&self, focused_panel: usize, name: Option<String>) -> String {
        let target_name = if let Some(name) = name {
            name
        } else {
            let Some(panel) = self.panels.get(focused_panel) else {
                return no_panel_focused_error();
            };
            panel.datasource.clone()
        };
        let Some(datasource) = self.datasources.get(&target_name) else {
            return format!("{}: unknown datasource \"{target_name}\"", ErrorCode::E101);
        };
        let ds_name = target_name;
        let adapter = Arc::clone(&datasource.adapter);
        let update_tx = self.update_tx.clone();
        tokio::spawn(async move {
            let result = adapter
                .metric_names()
                .await
                .map_err(|err| CommandError::new(ErrorCode::E106, err.to_string(), None));
            let _ = update_tx
                .send(SessionUpdate::DsMetrics {
                    datasource: ds_name,
                    result,
                })
                .await;
        });
        "ds metrics: fetching…".to_string()
    }

    /// `!<command>` (`docs/specs/open.md` Section K). Runs `command`
    /// through `$SHELL -c` (falling back to `/bin/sh` if `$SHELL` isn't
    /// set), same as every other REPL that shells out — no allow-list,
    /// no confirmation prompt: this is a local dev/ops tool driven by
    /// hand, deliberately trusted the same as a real shell. Runs on a
    /// blocking thread (`tokio::task::spawn_blocking`) since
    /// `std::process::Command::output()` blocks; the render loop never
    /// stalls waiting on it, same async-ack shape `spawn_ad_hoc_query`/
    /// `spawn_ds_metrics_query` already use. An empty command (bare
    /// `!`) is rejected synchronously — nothing to run, no task spawned.
    /// `pub`, unlike its two siblings above: those are only ever reached
    /// through `execute_command`'s `Command` dispatch (same file),
    /// `!` is parsed as `ShellInput::Shell` in `dash9-tui::shell`
    /// (`docs/specs/open.md` Section C) and handled directly by each
    /// `CommandHandler::execute` in `open.rs`/`assist_bridge.rs`,
    /// same as `export_panel`.
    pub fn spawn_shell_command(&self, command: &str) -> String {
        if command.is_empty() {
            return "!: no command given".to_string();
        }
        let update_tx = self.update_tx.clone();
        let command_for_task = command.to_string();
        let command_for_result = command_for_task.clone();
        tokio::spawn(async move {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
            let output = tokio::task::spawn_blocking(move || {
                std::process::Command::new(&shell)
                    .arg("-c")
                    .arg(&command_for_task)
                    .output()
            })
            .await;
            let result = match output {
                Ok(Ok(output)) => Ok(ShellOutput {
                    status: output.status.code(),
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                }),
                Ok(Err(err)) => Err(err.to_string()),
                Err(err) => Err(err.to_string()),
            };
            let _ = update_tx
                .send(SessionUpdate::Shell {
                    command: command_for_result,
                    result,
                })
                .await;
        });
        format!("! {command}: running…")
    }

    /// `:save <format> [path]`. Exports the focused panel's *current*
    /// data — one code path for every panel type, via
    /// `dash9_tui::table_for_export`'s existing fallback (native
    /// `Table` if the frame has one, else the same `series_as_table`
    /// synthesis the table-panel renderer already uses). PNG is
    /// deliberately not implemented (see `ExportFormat::Png`'s docs)
    /// — reported honestly, not faked.
    pub fn export_panel(
        &self,
        focused_panel: usize,
        format: ExportFormat,
        path: Option<&str>,
    ) -> String {
        let Some(panel) = self.panels.get(focused_panel) else {
            return no_panel_focused_error();
        };
        if format == ExportFormat::Png {
            return "PNG export is not available (no local renderer configured)".to_string();
        }
        let Some(result) = panel.last_result.as_ref() else {
            return format!("panel \"{}\" has no data yet", panel.title);
        };
        let frame = match result {
            Err(err) => return format!("panel \"{}\" last query failed: {err}", panel.title),
            Ok(frame) => frame,
        };
        let Some(table) = table_for_export(frame) else {
            return format!("panel \"{}\" has no data to export", panel.title);
        };
        let content = match format {
            ExportFormat::Csv => table_to_csv(&table),
            ExportFormat::Markdown => table_to_markdown(&table),
            ExportFormat::Png => unreachable!("returned above"),
        };

        let relative =
            path.map_or_else(|| default_export_path(&panel.title, format), str::to_string);
        let destination = match validate_workspace_relative_path(&self.workspace_root, &relative) {
            Ok(p) => p,
            Err(err) => return err.to_string(),
        };
        if let Some(parent) = destination.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                return format!("failed to create {}: {err}", parent.display());
            }
        }
        match std::fs::write(&destination, content) {
            Ok(()) => format!("saved to {}", destination.display()),
            Err(err) => format!("failed to write {}: {err}", destination.display()),
        }
    }

    fn save(&self, path: &str) -> String {
        let destination = match validate_workspace_relative_path(&self.workspace_root, path) {
            Ok(p) => p,
            Err(err) => return err.to_string(),
        };
        let Some(toml_text) = self.to_toml_string() else {
            return "failed to serialize dashboard".to_string();
        };
        match std::fs::write(&destination, toml_text) {
            Ok(()) => format!("saved to {}", destination.display()),
            Err(err) => format!("failed to write {}: {err}", destination.display()),
        }
    }

    /// The current session state serialized as dashboard TOML text —
    /// used both by `dash save` and (PR10) as `AssistContext`'s
    /// `dashboard_toml` field, so the assistant sees the live session,
    /// not just the file it was originally opened from. `None` only on
    /// a serialization failure (`toml::to_string` erroring), which
    /// `DashboardFile`'s shape makes essentially unreachable in
    /// practice — never on empty/missing data.
    pub fn to_toml_string(&self) -> Option<String> {
        toml::to_string(&self.to_dashboard_file()).ok()
    }

    fn to_dashboard_file(&self) -> DashboardFile {
        let mut datasource_names: Vec<&String> = self.datasources.keys().collect();
        datasource_names.sort();
        let datasources = datasource_names
            .into_iter()
            .map(|name| {
                let ds = &self.datasources[name];
                DatasourceSpec {
                    name: name.clone(),
                    type_: ds.datasource_type.to_string(),
                    url: ds.url.clone(),
                }
            })
            .collect();

        let panels = self
            .panels
            .iter()
            .map(|p| PanelSpec {
                title: p.title.clone(),
                type_: p.panel_type.to_string(),
                datasource: p.datasource.clone(),
                query: p.query.clone(),
                allow_empty: p.allow_empty,
                latency_budget: p.latency_budget.map(|d| d.to_string()),
                grid: p.grid,
                thresholds: p
                    .thresholds
                    .iter()
                    .map(|t| ThresholdSpec {
                        name: t.name.clone(),
                        op: t.op.to_string(),
                        value: t.value,
                    })
                    .collect(),
            })
            .collect();

        DashboardFile {
            dashboard: DashboardMeta {
                title: self.title.clone(),
                refresh: self.refresh().to_string(),
                default_range: self.range().to_string(),
                test_latency_budget: self.test_latency_budget.to_string(),
            },
            datasources,
            panels,
        }
    }

    /// `dash open`: on success, replaces the entire session (aborts
    /// every current poller and rebuilds from the newly loaded
    /// dashboard) — the caller must re-clamp `focused_panel` against
    /// the new `panels.len()` afterward, since the panel count may
    /// have changed. On failure, the current session keeps running
    /// unchanged — a bad `dash open` inside a live session must not
    /// kill the TUI, unlike the fatal startup load in `open::run`.
    fn open_dashboard(&mut self, path: &str) -> String {
        let destination = match validate_workspace_relative_path(&self.workspace_root, path) {
            Ok(p) => p,
            Err(err) => return err.to_string(),
        };
        let dashboard = match load_path(&destination).and_then(|file| validate(&file)) {
            Ok(d) => d,
            Err(err) => return format!("dashboard invalid: {err}"),
        };

        for handle in self.poller_handles.drain(..) {
            handle.abort();
        }
        self.generation += 1;
        self.title.clone_from(&dashboard.title);
        self.test_latency_budget = dashboard.test_latency_budget;
        self.datasources = build_live_datasources(&dashboard);
        self.panels = dashboard.panels.iter().map(LivePanel::from).collect();
        self.config_tx
            .send_modify(|cfg| *cfg = runtime_config_for(&dashboard));
        self.spawn_all_pollers();

        format!("opened {}", destination.display())
    }
}

fn runtime_config_for(dashboard: &ValidatedDashboard) -> RuntimeConfig {
    RuntimeConfig {
        range: dashboard.default_range,
        refresh: dashboard.refresh,
        panel_types: dashboard.panels.iter().map(|p| p.panel_type).collect(),
    }
}

fn build_live_datasources(dashboard: &ValidatedDashboard) -> HashMap<String, LiveDatasource> {
    dashboard
        .datasources
        .iter()
        .map(|ds| {
            // `dash9-core::validate()` only accepts `type =
            // "prometheus"` in v0.1 (SPEC.md C.1/D), so this is the
            // only adapter constructed today; a second datasource
            // type will need a match here.
            let adapter = Arc::new(PrometheusDatasource::new(ds.name.clone(), ds.url.clone()));
            let live = LiveDatasource {
                datasource_type: ds.datasource_type,
                url: ds.url.clone(),
                adapter,
            };
            (ds.name.clone(), live)
        })
        .collect()
}

fn no_panel_focused_error() -> String {
    format!(
        "{}: \"panel *\" issued with no panel focused",
        ErrorCode::E103
    )
}

/// `exports/<slugified-panel-title>-<unix-ms>.<ext>`, used when
/// `:save <format>` is given no explicit path. Not collision-proof
/// against two exports in the same millisecond, but good enough for
/// an interactive, one-at-a-time command.
fn default_export_path(panel_title: &str, format: ExportFormat) -> String {
    let slug: String = panel_title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    format!("exports/{slug}-{}.{}", epoch_ms_now(), format.extension())
}

fn format_ds_metrics_result(datasource: &str, result: Result<Vec<String>, CommandError>) -> String {
    match result {
        Ok(names) if names.is_empty() => format!("ds metrics {datasource}: (no metrics)"),
        Ok(names) => format!(
            "ds metrics {datasource}: {} metrics\n{}",
            names.len(),
            names.join(", ")
        ),
        Err(err) => format!("ds metrics {datasource}: {err}"),
    }
}

/// `stdout`/`stderr` are trimmed of trailing newlines (the shell
/// always adds one) but otherwise shown verbatim, in that order —
/// `stderr` last since it's usually the more interesting half on
/// failure and the output pane shows most-recent-relevant content at
/// the bottom-most-visible edge (`docs/specs/open.md` Section F). A
/// non-zero/missing status is prefixed to make failure obvious without
/// having to read the whole body.
fn format_shell_result(command: &str, result: Result<ShellOutput, String>) -> String {
    let output = match result {
        Err(err) => return format!("! {command}: failed to run: {err}"),
        Ok(output) => output,
    };
    let mut out = match output.status {
        Some(0) => format!("! {command}"),
        Some(code) => format!("! {command} (exit {code})"),
        None => format!("! {command} (terminated by signal)"),
    };
    let stdout = output.stdout.trim_end_matches('\n');
    let stderr = output.stderr.trim_end_matches('\n');
    if !stdout.is_empty() {
        out.push('\n');
        out.push_str(stdout);
    }
    if !stderr.is_empty() {
        out.push('\n');
        out.push_str(stderr);
    }
    out
}

fn format_ad_hoc_result(query: &str, frame: &Frame) -> String {
    if frame.is_empty() {
        format!("q {query}: (no data)")
    } else {
        let series_count = frame.series.len();
        let latest: Vec<String> = frame
            .series
            .iter()
            .filter_map(|s| s.points.last())
            .map(|p| format!("{:.3}", p.value))
            .collect();
        format!(
            "q {query}: {series_count} series — latest: [{}]",
            latest.join(", ")
        )
    }
}

/// One polling task per panel: reads `config_rx` fresh each iteration
/// (rather than capturing `range`/`refresh`/`panel_type` at spawn
/// time) so a live edit to any of the three takes effect on this
/// task's very next iteration without needing to be aborted and
/// respawned — see module docs. `RefreshInterval::Off` means fetch
/// once and stop, matching PR8's behavior and the verb's SPEC.md B.3
/// semantics.
fn spawn_poller(
    panel_index: usize,
    generation: u64,
    query: String,
    datasource: Arc<PrometheusDatasource>,
    mut config_rx: watch::Receiver<RuntimeConfig>,
    update_tx: mpsc::Sender<SessionUpdate>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let cfg = config_rx.borrow_and_update().clone();
            let panel_type = cfg.panel_types[panel_index];
            let now_ms = epoch_ms_now();
            let result =
                execute_panel_query(&datasource, panel_type, &query, cfg.range, now_ms).await;

            let update = SessionUpdate::Panel {
                generation,
                panel_index,
                result,
            };
            if update_tx.send(update).await.is_err() {
                return; // Render loop exited; stop polling.
            }

            let sleep_duration = match cfg.refresh {
                RefreshInterval::Duration(d) => {
                    StdDuration::from_millis(u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
                }
                RefreshInterval::Off => return,
            };
            tokio::select! {
                () = tokio::time::sleep(sleep_duration) => {}
                _ = config_rx.changed() => {}
            }
        }
    })
}

/// Executes one parsed `Command` against a live session, mutating it
/// as needed, and returns a human-readable outcome for the log.
/// `Command::Quit` is handled by the caller before this is invoked —
/// it ends the session rather than mutating one.
pub fn execute_command(session: &mut LiveSession, focused_panel: usize, cmd: Command) -> String {
    match cmd {
        Command::DsAdd {
            name,
            datasource_type,
            url,
        } => {
            if session.datasources.contains_key(&name) {
                return format!("{}: duplicate datasource name \"{name}\"", ErrorCode::E102);
            }
            let adapter = Arc::new(PrometheusDatasource::new(name.clone(), url.clone()));
            session.datasources.insert(
                name.clone(),
                LiveDatasource {
                    datasource_type,
                    url,
                    adapter,
                },
            );
            format!("datasource \"{name}\" added")
        }
        Command::DsList => {
            if session.datasources.is_empty() {
                "no datasources configured".to_string()
            } else {
                let mut names: Vec<&String> = session.datasources.keys().collect();
                names.sort();
                names
                    .into_iter()
                    .map(|name| {
                        let ds = &session.datasources[name];
                        format!("{name}: {} {}", ds.datasource_type, ds.url)
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        }
        Command::DsMetrics { name } => session.spawn_ds_metrics_query(focused_panel, name),
        Command::Q { query } => session.spawn_ad_hoc_query(focused_panel, &query),
        Command::PanelType { panel_type } => {
            if session.panels.is_empty() {
                return no_panel_focused_error();
            }
            session.panels[focused_panel].panel_type = panel_type;
            session.set_panel_type(focused_panel, panel_type);
            format!("panel type set to {panel_type}")
        }
        Command::PanelThreshold { name, op, value } => {
            if session.panels.is_empty() {
                return no_panel_focused_error();
            }
            let thresholds = &mut session.panels[focused_panel].thresholds;
            if let Some(existing) = thresholds.iter_mut().find(|t| t.name == name) {
                existing.op = op;
                existing.value = value;
            } else {
                thresholds.push(ValidatedThreshold {
                    name: name.clone(),
                    op,
                    value,
                });
            }
            format!("threshold \"{name}\" set to {op} {value}")
        }
        Command::PanelTitle { text } => {
            if session.panels.is_empty() {
                return no_panel_focused_error();
            }
            session.panels[focused_panel].title.clone_from(&text);
            format!("panel title set to \"{text}\"")
        }
        Command::Range { duration } => {
            session.set_range(duration);
            format!("range set to {duration}")
        }
        Command::Refresh { interval } => {
            session.set_refresh(interval);
            format!("refresh set to {interval}")
        }
        Command::DashSave { path } => session.save(&path),
        Command::DashOpen { path } => session.open_dashboard(&path),
        Command::Quit => {
            unreachable!("Command::Quit is handled by the caller before execute_command runs")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash9_core::{
        DurationUnit, FrameKind, FrameMeta, Point, Series, ThresholdOp, ValidatedDatasource,
        ValidatedPanel,
    };
    use tempfile::TempDir;

    fn sample_panel(title: &str) -> ValidatedPanel {
        ValidatedPanel {
            title: title.to_string(),
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
        }
    }

    fn sample_dashboard(panels: Vec<ValidatedPanel>) -> ValidatedDashboard {
        ValidatedDashboard {
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
            // Unroutable on purpose: these tests exercise
            // `execute_command`'s dispatch/mutation logic, never a
            // poller's actual fetch, so this never needs to resolve.
            datasources: vec![ValidatedDatasource {
                name: "prom".to_string(),
                datasource_type: DatasourceType::Prometheus,
                url: "http://127.0.0.1:1".to_string(),
            }],
            panels,
        }
    }

    /// Builds a session backed by a real temp workspace (so
    /// `dash save`/`dash open` path validation has somewhere real to
    /// resolve against) and a background `update_tx` receiver the
    /// test just drops — panel pollers will fail against the
    /// unroutable URL above and their results are simply never read.
    fn session_with(panels: Vec<ValidatedPanel>) -> (LiveSession, TempDir) {
        let dashboard = sample_dashboard(panels);
        let workspace = tempfile::tempdir().unwrap();
        let (tx, _rx) = mpsc::channel(16);
        let session = LiveSession::new(&dashboard, workspace.path().to_path_buf(), tx);
        (session, workspace)
    }

    fn minutes(magnitude: u64) -> Duration {
        Duration {
            magnitude,
            unit: DurationUnit::Minutes,
        }
    }

    #[tokio::test]
    async fn range_and_refresh_update_live_config() {
        let (mut session, _workspace) = session_with(vec![sample_panel("CPU")]);

        execute_command(
            &mut session,
            0,
            Command::Range {
                duration: minutes(5),
            },
        );
        assert_eq!(session.range(), minutes(5));

        execute_command(
            &mut session,
            0,
            Command::Refresh {
                interval: RefreshInterval::Off,
            },
        );
        assert_eq!(session.refresh(), RefreshInterval::Off);
    }

    #[tokio::test]
    async fn panel_type_updates_the_focused_panel_and_runtime_config() {
        let (mut session, _workspace) = session_with(vec![sample_panel("CPU")]);
        execute_command(
            &mut session,
            0,
            Command::PanelType {
                panel_type: PanelType::Gauge,
            },
        );
        assert_eq!(session.panels[0].panel_type, PanelType::Gauge);
        assert_eq!(session.config_rx.borrow().panel_types[0], PanelType::Gauge);
    }

    #[tokio::test]
    async fn panel_threshold_adds_then_updates_by_name() {
        let (mut session, _workspace) = session_with(vec![sample_panel("CPU")]);
        execute_command(
            &mut session,
            0,
            Command::PanelThreshold {
                name: "warn".to_string(),
                op: ThresholdOp::Gte,
                value: 0.5,
            },
        );
        assert_eq!(session.panels[0].thresholds.len(), 1);
        assert!((session.panels[0].thresholds[0].value - 0.5).abs() < f64::EPSILON);

        execute_command(
            &mut session,
            0,
            Command::PanelThreshold {
                name: "warn".to_string(),
                op: ThresholdOp::Gte,
                value: 0.9,
            },
        );
        assert_eq!(
            session.panels[0].thresholds.len(),
            1,
            "updated, not appended"
        );
        assert!((session.panels[0].thresholds[0].value - 0.9).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn panel_verbs_on_a_panel_less_session_report_e103() {
        let (mut session, _workspace) = session_with(vec![]);
        let outcome = execute_command(
            &mut session,
            0,
            Command::PanelTitle {
                text: "x".to_string(),
            },
        );
        assert!(outcome.contains("E103"), "{outcome}");
    }

    #[tokio::test]
    async fn ds_add_duplicate_name_is_e102() {
        let (mut session, _workspace) = session_with(vec![]);
        let outcome = execute_command(
            &mut session,
            0,
            Command::DsAdd {
                name: "prom".to_string(), // already present, see sample_dashboard
                datasource_type: DatasourceType::Prometheus,
                url: "http://example.invalid".to_string(),
            },
        );
        assert!(outcome.contains("E102"), "{outcome}");
    }

    #[tokio::test]
    async fn ds_add_new_name_is_listed() {
        let (mut session, _workspace) = session_with(vec![]);
        execute_command(
            &mut session,
            0,
            Command::DsAdd {
                name: "loki".to_string(),
                datasource_type: DatasourceType::Prometheus,
                url: "http://example.invalid".to_string(),
            },
        );
        assert!(session.datasources.contains_key("loki"));
        let listed = execute_command(&mut session, 0, Command::DsList);
        assert!(
            listed.contains("loki") && listed.contains("prom"),
            "{listed}"
        );
    }

    #[tokio::test]
    async fn q_with_no_panels_reports_e103() {
        let (mut session, _workspace) = session_with(vec![]);
        let outcome = execute_command(
            &mut session,
            0,
            Command::Q {
                query: "up".to_string(),
            },
        );
        assert!(outcome.contains("E103"), "{outcome}");
    }

    #[tokio::test]
    async fn ds_metrics_named_unknown_datasource_is_e101() {
        let (mut session, _workspace) = session_with(vec![]);
        let outcome = execute_command(
            &mut session,
            0,
            Command::DsMetrics {
                name: Some("bogus".to_string()),
            },
        );
        assert!(outcome.contains("E101"), "{outcome}");
    }

    #[tokio::test]
    async fn ds_metrics_bare_with_no_panels_reports_e103() {
        let (mut session, _workspace) = session_with(vec![]);
        let outcome = execute_command(&mut session, 0, Command::DsMetrics { name: None });
        assert!(outcome.contains("E103"), "{outcome}");
    }

    #[tokio::test]
    async fn ds_metrics_bare_uses_the_focused_panels_datasource_and_starts_fetching() {
        let (mut session, _workspace) = session_with(vec![sample_panel("CPU")]);
        let outcome = execute_command(&mut session, 0, Command::DsMetrics { name: None });
        assert!(outcome.contains("fetching"), "{outcome}");
    }

    #[tokio::test]
    async fn ds_metrics_named_known_datasource_starts_fetching() {
        let (mut session, _workspace) = session_with(vec![]);
        let outcome = execute_command(
            &mut session,
            0,
            Command::DsMetrics {
                name: Some("prom".to_string()),
            },
        );
        assert!(outcome.contains("fetching"), "{outcome}");
    }

    #[test]
    fn format_ds_metrics_result_lists_names_and_count() {
        let text =
            format_ds_metrics_result("prom", Ok(vec!["up".to_string(), "node_load1".to_string()]));
        assert!(text.contains("2 metrics"), "{text}");
        assert!(text.contains("up, node_load1"), "{text}");
    }

    #[test]
    fn format_ds_metrics_result_with_no_metrics_says_so() {
        let text = format_ds_metrics_result("prom", Ok(vec![]));
        assert!(text.contains("(no metrics)"), "{text}");
    }

    #[test]
    fn format_ds_metrics_result_with_an_error_reports_it() {
        let text = format_ds_metrics_result(
            "prom",
            Err(CommandError::new(ErrorCode::E106, "boom", None)),
        );
        assert!(text.contains("boom"), "{text}");
    }

    #[test]
    fn format_shell_result_success_shows_stdout_with_no_exit_annotation() {
        let text = format_shell_result(
            "echo hi",
            Ok(ShellOutput {
                status: Some(0),
                stdout: "hi\n".to_string(),
                stderr: String::new(),
            }),
        );
        assert_eq!(text, "! echo hi\nhi");
    }

    #[test]
    fn format_shell_result_nonzero_exit_is_annotated_and_shows_stderr() {
        let text = format_shell_result(
            "false-ish",
            Ok(ShellOutput {
                status: Some(1),
                stdout: String::new(),
                stderr: "boom\n".to_string(),
            }),
        );
        assert!(text.contains("(exit 1)"), "{text}");
        assert!(text.contains("boom"), "{text}");
    }

    #[test]
    fn format_shell_result_signal_terminated_has_no_exit_code() {
        let text = format_shell_result(
            "kill -9 $$",
            Ok(ShellOutput {
                status: None,
                stdout: String::new(),
                stderr: String::new(),
            }),
        );
        assert!(text.contains("terminated by signal"), "{text}");
    }

    #[test]
    fn format_shell_result_spawn_failure_reports_it() {
        let text = format_shell_result("whatever", Err("No such file or directory".to_string()));
        assert!(text.contains("failed to run"), "{text}");
        assert!(text.contains("No such file or directory"), "{text}");
    }

    #[tokio::test]
    async fn apply_update_ds_metrics_pushes_a_result_log_line() {
        let (mut session, _workspace) = session_with(vec![]);
        let mut log = Vec::new();
        session.apply_update(
            SessionUpdate::DsMetrics {
                datasource: "prom".to_string(),
                result: Err(CommandError::new(ErrorCode::E106, "boom", None)),
            },
            &mut log,
        );
        match log.as_slice() {
            [LogLine::Result(text)] => assert!(text.contains("boom"), "{text}"),
            other => panic!("expected exactly one Result log line, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dash_save_then_reload_round_trips_current_state() {
        let (mut session, workspace) = session_with(vec![sample_panel("CPU")]);
        execute_command(
            &mut session,
            0,
            Command::PanelTitle {
                text: "CPU (renamed)".to_string(),
            },
        );
        execute_command(
            &mut session,
            0,
            Command::Range {
                duration: minutes(10),
            },
        );

        let outcome = execute_command(
            &mut session,
            0,
            Command::DashSave {
                path: "scratch.toml".to_string(),
            },
        );
        assert!(outcome.starts_with("saved to"), "{outcome}");

        let saved = load_path(&workspace.path().join("scratch.toml")).unwrap();
        let reloaded = validate(&saved).unwrap();
        assert_eq!(reloaded.panels[0].title, "CPU (renamed)");
        assert_eq!(reloaded.default_range, minutes(10));
    }

    #[tokio::test]
    async fn dash_save_rejects_a_path_escaping_the_workspace() {
        let (mut session, _workspace) = session_with(vec![sample_panel("CPU")]);
        let outcome = execute_command(
            &mut session,
            0,
            Command::DashSave {
                path: "../escape.toml".to_string(),
            },
        );
        assert!(outcome.contains("E107"), "{outcome}");
    }

    #[tokio::test]
    async fn dash_open_missing_file_keeps_the_current_session() {
        let (mut session, _workspace) = session_with(vec![sample_panel("CPU")]);
        let outcome = execute_command(
            &mut session,
            0,
            Command::DashOpen {
                path: "does-not-exist.toml".to_string(),
            },
        );
        assert!(outcome.contains("dashboard invalid"), "{outcome}");
        assert_eq!(session.panels.len(), 1, "current session must be untouched");
    }

    const OTHER_DASHBOARD_TOML: &str = r#"
[dashboard]
title = "Other"
refresh = "30s"
default_range = "1h"

[[datasources]]
name = "prom"
type = "prometheus"
url = "http://127.0.0.1:1"

[[panels]]
title = "A"
type = "stat"
datasource = "prom"
query = "up"
grid = { row = 0, col = 0, w = 1, h = 1 }

[[panels]]
title = "B"
type = "stat"
datasource = "prom"
query = "up"
grid = { row = 0, col = 1, w = 1, h = 1 }
"#;

    #[tokio::test]
    async fn dash_open_success_replaces_the_whole_session() {
        let (mut session, workspace) = session_with(vec![sample_panel("CPU")]);
        std::fs::write(workspace.path().join("other.toml"), OTHER_DASHBOARD_TOML).unwrap();

        let outcome = execute_command(
            &mut session,
            0,
            Command::DashOpen {
                path: "other.toml".to_string(),
            },
        );
        assert!(outcome.starts_with("opened"), "{outcome}");
        assert_eq!(session.title, "Other");
        assert_eq!(session.panels.len(), 2);
        assert_eq!(session.panels[0].title, "A");
        assert_eq!(session.panels[1].title, "B");
    }

    fn empty_frame(query: &str) -> Frame {
        Frame {
            kind: FrameKind::InstantVector,
            series: vec![],
            table: None,
            meta: FrameMeta {
                query: query.to_string(),
                datasource: "prom".to_string(),
                executed_at_ms: 0,
                warnings: vec![],
            },
        }
    }

    #[tokio::test]
    async fn apply_update_drops_stale_generation_panel_results() {
        let (mut session, _workspace) = session_with(vec![sample_panel("CPU")]);
        let mut log = Vec::new();

        session.apply_update(
            SessionUpdate::Panel {
                generation: 999, // stale: session.generation is 0
                panel_index: 0,
                result: Ok(empty_frame("up")),
            },
            &mut log,
        );
        assert!(session.panels[0].last_result.is_none());

        session.apply_update(
            SessionUpdate::Panel {
                generation: 0,
                panel_index: 0,
                result: Ok(empty_frame("up")),
            },
            &mut log,
        );
        assert!(session.panels[0].last_result.is_some());
    }

    #[tokio::test]
    async fn shell_command_empty_is_rejected_synchronously_with_no_task_spawned() {
        let (session, _workspace) = session_with(vec![]);
        let outcome = session.spawn_shell_command("");
        assert!(outcome.contains("no command given"), "{outcome}");
    }

    #[tokio::test]
    async fn shell_command_non_empty_acks_immediately_and_runs_in_the_background() {
        let (session, _workspace) = session_with(vec![]);
        let outcome = session.spawn_shell_command("echo hi");
        assert!(outcome.contains("running"), "{outcome}");
    }

    #[tokio::test]
    async fn apply_update_shell_pushes_a_result_log_line() {
        let (mut session, _workspace) = session_with(vec![]);
        let mut log = Vec::new();
        session.apply_update(
            SessionUpdate::Shell {
                command: "echo hi".to_string(),
                result: Ok(ShellOutput {
                    status: Some(0),
                    stdout: "hi\n".to_string(),
                    stderr: String::new(),
                }),
            },
            &mut log,
        );
        match log.as_slice() {
            [LogLine::Result(text)] => assert!(text.contains("hi"), "{text}"),
            other => panic!("expected exactly one Result log line, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn apply_update_ad_hoc_pushes_a_result_log_line() {
        let (mut session, _workspace) = session_with(vec![]);
        let mut log = Vec::new();
        session.apply_update(
            SessionUpdate::AdHoc {
                query: "up".to_string(),
                result: Err(CommandError::new(ErrorCode::E106, "boom", None)),
            },
            &mut log,
        );
        match log.as_slice() {
            [LogLine::Result(text)] => assert!(text.contains("boom"), "{text}"),
            other => panic!("expected exactly one Result log line, got {other:?}"),
        }
    }

    fn non_empty_frame() -> Frame {
        let mut labels = std::collections::BTreeMap::new();
        labels.insert("job".to_string(), "node".to_string());
        Frame {
            kind: FrameKind::InstantVector,
            series: vec![Series {
                labels,
                points: vec![Point {
                    timestamp_ms: 0,
                    value: 0.5,
                }],
            }],
            table: None,
            meta: FrameMeta {
                query: "up".to_string(),
                datasource: "prom".to_string(),
                executed_at_ms: 0,
                warnings: vec![],
            },
        }
    }

    #[tokio::test]
    async fn export_panel_on_empty_session_reports_e103() {
        let (session, _workspace) = session_with(vec![]);
        let outcome = session.export_panel(0, ExportFormat::Csv, None);
        assert!(outcome.contains("E103"), "{outcome}");
    }

    #[tokio::test]
    async fn export_panel_with_no_data_yet_says_so() {
        let (session, _workspace) = session_with(vec![sample_panel("CPU")]);
        let outcome = session.export_panel(0, ExportFormat::Csv, None);
        assert!(outcome.contains("no data yet"), "{outcome}");
    }

    #[tokio::test]
    async fn export_panel_with_a_failed_query_reports_the_error() {
        let (mut session, _workspace) = session_with(vec![sample_panel("CPU")]);
        session.panels[0].last_result = Some(Err(CommandError::new(ErrorCode::E106, "boom", None)));
        let outcome = session.export_panel(0, ExportFormat::Csv, None);
        assert!(outcome.contains("boom"), "{outcome}");
    }

    #[tokio::test]
    async fn export_panel_png_is_never_implemented_regardless_of_data_state() {
        let (session, _workspace) = session_with(vec![sample_panel("CPU")]);
        let outcome = session.export_panel(0, ExportFormat::Png, None);
        assert!(outcome.contains("not available"), "{outcome}");
    }

    #[tokio::test]
    async fn export_panel_csv_writes_a_file_with_a_default_path() {
        let (mut session, workspace) = session_with(vec![sample_panel("CPU Usage")]);
        session.panels[0].last_result = Some(Ok(non_empty_frame()));

        let outcome = session.export_panel(0, ExportFormat::Csv, None);
        assert!(outcome.starts_with("saved to"), "{outcome}");

        let exports_dir = workspace.path().join("exports");
        let entries: Vec<_> = std::fs::read_dir(&exports_dir).unwrap().collect();
        assert_eq!(entries.len(), 1, "expected exactly one exported file");
        let written = std::fs::read_to_string(entries[0].as_ref().unwrap().path()).unwrap();
        assert!(written.starts_with("job,value\n"));
        assert!(written.contains("node,0.500"));
    }

    #[tokio::test]
    async fn export_panel_markdown_with_explicit_path() {
        let (mut session, workspace) = session_with(vec![sample_panel("CPU")]);
        session.panels[0].last_result = Some(Ok(non_empty_frame()));

        let outcome = session.export_panel(0, ExportFormat::Markdown, Some("scratch.md"));
        assert!(outcome.starts_with("saved to"), "{outcome}");
        let written = std::fs::read_to_string(workspace.path().join("scratch.md")).unwrap();
        assert!(written.starts_with("| job | value |"));
    }

    #[tokio::test]
    async fn export_panel_rejects_a_path_escaping_the_workspace() {
        let (mut session, _workspace) = session_with(vec![sample_panel("CPU")]);
        session.panels[0].last_result = Some(Ok(non_empty_frame()));
        let outcome = session.export_panel(0, ExportFormat::Csv, Some("../escape.csv"));
        assert!(outcome.contains("E107"), "{outcome}");
    }
}
