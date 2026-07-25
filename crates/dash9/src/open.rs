//! `dash9 open`: the live, interactive multi-panel viewer with a
//! command bar, status bar, and (with `--assist`) natural-language
//! input. Loads a dashboard TOML, lays out its panels on the
//! 12-column grid (SPEC.md C.1), polls each panel's datasource live,
//! renders all four panel types, and runs the full command grammar
//! (SPEC.md Section B) against a live, mutable session.
//!
//! The event loop and key-handling are `dash9_tui::shell`'s
//! `ShellState`/`CommandHandler` — a pure state machine (no terminal/
//! filesystem/network I/O, fully unit-tested there) calling into a
//! `CommandHandler` implementation that does the real work. This
//! module supplies two implementations: [`GrammarOnlyHandler`] (no
//! assist awareness at all, used by `run_plain`) and
//! `assist_bridge::AssistHandler` (`#[cfg(feature = "assist")]`, used
//! by `run_with_assist`). Both drive the same [`shell_loop`], so the
//! render loop itself is written once — `run_plain`/`run_with_assist`
//! are now thin constructors, not two parallel copies of the loop.
//!
//! `CommandHandler` (defined in `dash9-tui`) has no way to expose
//! `LiveSession`-typed data — `dash9-tui` doesn't and can't depend on
//! this binary crate's types — so panel-grid drawing goes through a
//! second, local, non-generic-crate-boundary trait, [`HasSession`],
//! implemented by both handlers alongside `CommandHandler`.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use crossterm::event::{self, Event};
use dash9_core::{load_path, validate, Command};
use dash9_tui::chart::{ChartModel, ChartViewState};
use dash9_tui::shell::{CommandHandler, CommandResponse, ShellInput, ShellState};
use dash9_tui::{
    draw_chart, draw_command_bar, draw_gauge, draw_stat, draw_status_bar, draw_table, grid_layout,
    help_text, series_as_table, theme, StatusBarModel,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc;

use crate::live_session::{execute_command, LivePanel, LiveSession, SessionUpdate};
use crate::log_recorder::LogRecorder;

const TICK: StdDuration = StdDuration::from_millis(250);
const CHANNEL_CAPACITY: usize = 64;
const STATUS_BAR_HEIGHT: u16 = 1;

pub fn run(path: &Path, assist: bool) -> anyhow::Result<()> {
    if assist {
        return run_with_assist(path);
    }
    run_plain(path)
}

fn run_plain(path: &Path) -> anyhow::Result<()> {
    let dashboard = load_path(path)
        .and_then(|file| validate(&file))
        .map_err(|err| anyhow::anyhow!("dashboard invalid: {err}"))?;
    let workspace_root = std::env::current_dir()?;

    let (tx, rx) = mpsc::channel::<SessionUpdate>(CHANNEL_CAPACITY);
    let session = LiveSession::new(&dashboard, workspace_root.clone(), tx);
    let recorder = Arc::new(Mutex::new(LogRecorder::new(workspace_root)));
    let handler = GrammarOnlyHandler {
        session,
        update_rx: rx,
        recorder: Arc::clone(&recorder),
    };
    shell_loop(handler, ShellState::default(), &recorder)
}

#[cfg(not(feature = "assist"))]
fn run_with_assist(_path: &Path) -> anyhow::Result<()> {
    anyhow::bail!(
        "dash9 was built without the `assist` feature; rebuild with `cargo build --features assist`"
    )
}

#[cfg(feature = "assist")]
fn run_with_assist(path: &Path) -> anyhow::Result<()> {
    let dashboard = load_path(path)
        .and_then(|file| validate(&file))
        .map_err(|err| anyhow::anyhow!("dashboard invalid: {err}"))?;
    let workspace_root = std::env::current_dir()?;

    let (tx, rx) = mpsc::channel::<SessionUpdate>(CHANNEL_CAPACITY);
    let session = LiveSession::new(&dashboard, workspace_root.clone(), tx);
    let recorder = Arc::new(Mutex::new(LogRecorder::new(workspace_root.clone())));

    let mut state = ShellState::default();
    let (handler, startup_message) = crate::assist_bridge::AssistHandler::new(
        session,
        rx,
        workspace_root,
        Arc::clone(&recorder),
    );
    if let Some(message) = startup_message {
        state.log.push(dash9_core::LogLine::Result(message));
    }
    shell_loop(handler, state, &recorder)
}

/// Exposes the `LiveSession` a `CommandHandler` implementation wraps,
/// purely so the (already-existing, unchanged) panel-grid drawing
/// functions below can read panel data. Separate from
/// `CommandHandler` itself because `dash9-tui` cannot know about
/// `LiveSession` — see module docs.
pub(crate) trait HasSession {
    fn session(&self) -> &LiveSession;
}

/// The shared render loop: identical for `run_plain` and
/// `run_with_assist` now, generic over whichever `CommandHandler`
/// implementation is driving it. `recorder` is the same handle the
/// handler holds (see [`GrammarOnlyHandler`]/`AssistHandler`) — the
/// handler owns turning `/record on|off` into an open/closed file,
/// this loop owns noticing every new `state.log` line and offering it
/// to the recorder, since that's the one place that sees every line
/// (including ones `ShellState` itself adds, like the echoed command
/// text, which never passes through a handler at all).
fn shell_loop<H: CommandHandler + HasSession>(
    mut handler: H,
    mut state: ShellState,
    recorder: &Arc<Mutex<LogRecorder>>,
) -> anyhow::Result<()> {
    ratatui::run(|terminal| -> anyhow::Result<()> {
        loop {
            let before = state.log.len();
            state.apply_poll(&mut handler);
            record_new_lines(&state, recorder, before);

            terminal.draw(|f| {
                let status = handler.status_bar();
                draw_session(f, f.area(), &state, handler.session(), &status);
            })?;

            if !event::poll(TICK)? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            let before = state.log.len();
            let should_quit = state.handle_key(key, &mut handler);
            record_new_lines(&state, recorder, before);
            if should_quit {
                return Ok(());
            }
        }
    })
}

/// Offers every log line added since `before` to the recorder — a
/// no-op on its end unless `/record on` is active. `before`/`after`
/// bracket a single `apply_poll`/`handle_key` call, so this is immune
/// to `ShellState`'s own `MAX_LOG_LINES` trimming shifting indices
/// across ticks (nothing persists between calls); it would only miss
/// lines if one single call added enough entries to itself trigger a
/// trim, which needs hundreds of lines from one keypress or poll
/// drain — never happens in practice.
fn record_new_lines(state: &ShellState, recorder: &Arc<Mutex<LogRecorder>>, before: usize) {
    let after = state.log.len();
    if after <= before {
        return;
    }
    lock_recorder(recorder).record(&state.log[before..after]);
}

/// A poisoned lock here would mean an earlier panic happened while
/// holding it — recovering the guard anyway rather than propagating
/// the poison keeps one bad recorder write from taking down the
/// entire render loop over what's fundamentally a best-effort side
/// channel, not session-critical state.
fn lock_recorder(recorder: &Mutex<LogRecorder>) -> std::sync::MutexGuard<'_, LogRecorder> {
    recorder
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The `run_plain` handler: no `dash9-assist` awareness at all. Every
/// AI-only `ShellInput` variant reports the same "requires --assist"
/// message; `NaturalLanguage` (any line with no leading `/`) reports
/// that natural language itself needs `--assist`.
struct GrammarOnlyHandler {
    session: LiveSession,
    update_rx: mpsc::Receiver<SessionUpdate>,
    recorder: Arc<Mutex<LogRecorder>>,
}

impl HasSession for GrammarOnlyHandler {
    fn session(&self) -> &LiveSession {
        &self.session
    }
}

impl CommandHandler for GrammarOnlyHandler {
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
            ShellInput::RecordingStatus => {
                CommandResponse::result(lock_recorder(&self.recorder).status())
            }
            ShellInput::SetRecording { on, path } => {
                CommandResponse::result(lock_recorder(&self.recorder).set(on, path))
            }
            ShellInput::NaturalLanguage(_) => CommandResponse::result(
                "natural language requires --assist (or prefix a command with /, see /help)"
                    .to_string(),
            ),
            ShellInput::ModelStatus
            | ShellInput::ModelSwitch(_)
            | ShellInput::ToggleAssist
            | ShellInput::AssistStatus
            | ShellInput::SetAssist(_) => {
                CommandResponse::result("AI features require --assist".to_string())
            }
        }
    }

    fn poll(&mut self, _focused_panel: usize) -> Option<CommandResponse> {
        match self.update_rx.try_recv() {
            Ok(update) => {
                let mut log_entries = Vec::new();
                self.session.apply_update(update, &mut log_entries);
                Some(CommandResponse {
                    log_entries,
                    ..CommandResponse::default()
                })
            }
            Err(_) => None,
        }
    }

    fn panel_count(&self) -> usize {
        self.session.panels.len()
    }

    fn status_bar(&self) -> StatusBarModel {
        status_bar_for(&self.session, None)
    }
}

/// Shared by both handlers so the "count panels, summarize
/// datasources, compute health" logic isn't duplicated between
/// `GrammarOnlyHandler` and `AssistHandler`.
pub(crate) fn status_bar_for(
    session: &LiveSession,
    assist: Option<dash9_tui::AssistStatusLine>,
) -> StatusBarModel {
    StatusBarModel {
        title: session.title.clone(),
        panel_count: session.panels.len(),
        datasource_summary: session.datasource_summary(),
        health: session.datasource_health(),
        assist,
    }
}

fn draw_session(
    frame: &mut Frame,
    area: Rect,
    state: &ShellState,
    session: &LiveSession,
    status: &StatusBarModel,
) {
    let grids: Vec<_> = session.panels.iter().map(|p| p.grid).collect();

    // The grid gets exactly the height its panels need, not whatever
    // space a `Min(0)`/stretch constraint would hand it — `grid_layout`
    // positions panels by absolute grid units, so a grid area taller
    // than its content just leaves a dead, unrendered gap below the
    // last panel row otherwise. The command bar (log + input) gets
    // everything left over instead, so it grows to use that space
    // rather than staying pinned to a fixed height.
    let [status_area, grid_area, bar_area] = Layout::vertical([
        Constraint::Length(STATUS_BAR_HEIGHT),
        Constraint::Length(dash9_tui::content_height(&grids)),
        Constraint::Min(0),
    ])
    .areas(area);
    draw_status_bar(frame, status_area, status);
    draw_dashboard(frame, grid_area, &grids, session, state.focused_panel);
    draw_command_bar(
        frame,
        bar_area,
        &state.log,
        state.input.as_deref(),
        &command_bar_hint(state, status),
        state.log_scroll,
    );
}

/// State-dependent footer text for the command-bar input line when
/// not actively typing: surfaces `y`/`n` only while a proposal is
/// genuinely pending, and `a` only when there's an assist handler to
/// toggle at all — never shown as options that would currently do
/// nothing.
fn command_bar_hint(state: &ShellState, status: &StatusBarModel) -> String {
    let mut hints = vec!["/command · text = AI", "Tab reaches this box", "/help"];
    if !state.pending_proposals.is_empty() {
        hints.push("y/n confirm proposal");
    }
    if status.assist.is_some() {
        hints.push("a toggle AI");
    }
    hints.push("q quit");
    hints.join(" · ")
}

fn draw_dashboard(
    frame: &mut Frame,
    area: Rect,
    grids: &[dash9_core::GridSpec],
    session: &LiveSession,
    focused_panel: usize,
) {
    let rects = grid_layout(area, grids);

    for (index, panel) in session.panels.iter().enumerate() {
        let rect = rects[index];
        if rect.width == 0 || rect.height == 0 {
            continue; // Positioned entirely off-screen (v1 has no scrolling).
        }
        draw_panel(frame, rect, panel);
        recolor_border(frame, rect, index == focused_panel);
    }
}

fn draw_panel(frame: &mut Frame, area: Rect, panel: &LivePanel) {
    let Some(result) = panel.last_result.as_ref() else {
        draw_placeholder(frame, area, &panel.title, "(loading…)");
        return;
    };
    let core_frame = match result {
        Err(err) => {
            draw_placeholder(frame, area, &panel.title, &err.to_string());
            return;
        }
        Ok(core_frame) => core_frame,
    };
    if core_frame.is_empty() {
        draw_placeholder(frame, area, &panel.title, "(no data)");
        return;
    }

    match panel.panel_type {
        dash9_core::PanelType::Timeseries
        | dash9_core::PanelType::Gauge
        | dash9_core::PanelType::Stat => {
            match ChartModel::project(
                &panel.title,
                core_frame,
                &panel.thresholds,
                &ChartViewState::default(),
                usize::from(area.width),
            ) {
                Ok(model) => match panel.panel_type {
                    dash9_core::PanelType::Timeseries => draw_chart(frame, area, &model),
                    dash9_core::PanelType::Gauge => draw_gauge(frame, area, &model),
                    dash9_core::PanelType::Stat => draw_stat(frame, area, &model),
                    dash9_core::PanelType::Table => unreachable!("handled in the outer match arm"),
                },
                Err(err) => draw_placeholder(frame, area, &panel.title, &err.to_string()),
            }
        }
        dash9_core::PanelType::Table => {
            // Prometheus never produces a native `FrameKind::Table`
            // (see `series_as_table`'s docs), so the common case here
            // is synthesizing one from the instant vector it actually
            // returned; `core_frame.table` is only used when some
            // future datasource does return one natively.
            match core_frame
                .table
                .clone()
                .or_else(|| series_as_table(core_frame))
            {
                Some(table) => draw_table(frame, area, &table, &panel.title),
                None => draw_placeholder(frame, area, &panel.title, "(no table data)"),
            }
        }
    }
}

fn draw_placeholder(frame: &mut Frame, area: Rect, title: &str, message: &str) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string());
    frame.render_widget(Paragraph::new(message.to_string()).block(block), area);
}

/// Recolors `area`'s border ring after its content has already drawn
/// its own bordered block — every panel-type draw function
/// (`draw_chart`/`draw_stat`/`draw_gauge`/`draw_table`) already builds
/// its own `Block`, so this restyles those cells directly via the
/// buffer rather than adding a `focused: bool` parameter to four
/// already-tested, already-used-by-`demo.rs` function signatures for
/// a feature that's cosmetic only.
fn recolor_border(frame: &mut Frame, area: Rect, focused: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let color = if focused { theme::FOCUS } else { theme::MUTED };
    let style = Style::default().fg(color);
    let buffer = frame.buffer_mut();

    for x in area.left()..area.right() {
        for y in [area.top(), area.bottom().saturating_sub(1)] {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_style(style);
            }
        }
    }
    for y in area.top()..area.bottom() {
        for x in [area.left(), area.right().saturating_sub(1)] {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_style(style);
            }
        }
    }
}
