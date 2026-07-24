//! `dash9 open`: the live, interactive multi-panel viewer with a
//! command bar. Loads a dashboard TOML, lays out its panels on the
//! 12-column grid (SPEC.md C.1), polls each panel's datasource live,
//! renders all four panel types, and — as of this revision — runs the
//! full command grammar (SPEC.md Section B) against a live, mutable
//! session: `ds add`, `panel type`/`threshold`/`title`, `range`,
//! `refresh`, `dash save`, `dash open`, ad-hoc `q`, `quit`.
//!
//! Session state and datasource polling live in `crate::live_session`
//! (`LiveSession`, `execute_command`); this module is the composition
//! root's UI layer — input handling, the session log, and draw-area
//! layout — plus the panel-content drawing carried over from the
//! read-only v1 (`draw_panel`/`draw_placeholder`/`recolor_border`).
//!
//! Natural-language input (routing unparseable text to
//! `dash9-assist`'s `AssistSession` when enabled) is a deliberate
//! follow-up, not part of this revision: seams left for it are typed
//! input failing `dash9_core::parse` (currently always an immediate
//! error) and `execute_command` (already the single place both a
//! human command and an eventual AI-proposed command would run).

use std::path::Path;
use std::time::Duration as StdDuration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use dash9_core::{
    load_path, validate, Command, CommandSource, LogLine, PanelType, SessionLogEntry,
};
use dash9_tui::chart::{ChartModel, ChartViewState};
use dash9_tui::{
    draw_chart, draw_command_bar, draw_gauge, draw_stat, draw_table, grid_layout, series_as_table,
    theme,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc;

use crate::datasources::epoch_ms_now;
use crate::live_session::{execute_command, LivePanel, LiveSession, SessionUpdate};

const TICK: StdDuration = StdDuration::from_millis(250);
const CHANNEL_CAPACITY: usize = 64;
const COMMAND_BAR_HEIGHT: u16 = 10;
const MAX_LOG_LINES: usize = 500;

pub fn run(path: &Path) -> anyhow::Result<()> {
    let dashboard = load_path(path)
        .and_then(|file| validate(&file))
        .map_err(|err| anyhow::anyhow!("dashboard invalid: {err}"))?;
    let workspace_root = std::env::current_dir()?;

    let (tx, mut rx) = mpsc::channel::<SessionUpdate>(CHANNEL_CAPACITY);
    let mut session = LiveSession::new(&dashboard, workspace_root, tx);

    let mut focused_panel = 0usize;
    let mut log: Vec<LogLine> = Vec::new();
    let mut input: Option<String> = None;

    ratatui::run(|terminal| -> anyhow::Result<()> {
        loop {
            while let Ok(update) = rx.try_recv() {
                session.apply_update(update, &mut log);
            }
            if focused_panel >= session.panels.len() {
                focused_panel = 0;
            }

            terminal.draw(|f| {
                draw_session(f, f.area(), &session, focused_panel, &log, input.as_deref());
            })?;

            if !event::poll(TICK)? {
                continue;
            }
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }

            if let Some(buffer) = input.as_mut() {
                match key.code {
                    KeyCode::Esc => input = None,
                    KeyCode::Enter => {
                        let text = buffer.clone();
                        input = None;
                        if submit_command(&mut session, &mut log, focused_panel, &text) {
                            return Ok(());
                        }
                    }
                    KeyCode::Backspace => {
                        buffer.pop();
                    }
                    KeyCode::Char(c) => buffer.push(c),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    KeyCode::Char(':') => input = Some(String::new()),
                    KeyCode::Tab if !session.panels.is_empty() => {
                        focused_panel = (focused_panel + 1) % session.panels.len();
                    }
                    KeyCode::BackTab if !session.panels.is_empty() => {
                        let len = session.panels.len();
                        focused_panel = (focused_panel + len - 1) % len;
                    }
                    _ => {}
                }
            }
        }
    })
}

/// Parses and runs one submitted command-bar line. Returns `true` if
/// the session should end (`quit`). Every submission — parseable or
/// not — is logged: a `SessionLogEntry` for what was typed, then a
/// `Result` line for the outcome, so a failed attempt is as visible
/// in the log as a successful one (matches `docs/specs/assist.md`
/// Section I's "no invisible action" rule, which this log format was
/// designed to satisfy even before an assistant is wired in).
fn submit_command(
    session: &mut LiveSession,
    log: &mut Vec<LogLine>,
    focused_panel: usize,
    text: &str,
) -> bool {
    log.push(LogLine::Command(SessionLogEntry {
        source: CommandSource::User,
        command_text: text.to_string(),
        timestamp_ms: epoch_ms_now(),
    }));

    let should_quit = match dash9_core::parse(text) {
        Ok(Command::Quit) => true,
        Ok(cmd) => {
            let outcome = execute_command(session, focused_panel, cmd);
            log.push(LogLine::Result(outcome));
            false
        }
        Err(err) => {
            log.push(LogLine::Result(err.to_string()));
            false
        }
    };

    if log.len() > MAX_LOG_LINES {
        let excess = log.len() - MAX_LOG_LINES;
        log.drain(0..excess);
    }
    should_quit
}

fn draw_session(
    frame: &mut Frame,
    area: Rect,
    session: &LiveSession,
    focused_panel: usize,
    log: &[LogLine],
    input: Option<&str>,
) {
    let [grid_area, bar_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(COMMAND_BAR_HEIGHT)]).areas(area);
    draw_dashboard(frame, grid_area, session, focused_panel);
    draw_command_bar(frame, bar_area, log, input);
}

fn draw_dashboard(frame: &mut Frame, area: Rect, session: &LiveSession, focused_panel: usize) {
    let grids: Vec<_> = session.panels.iter().map(|p| p.grid).collect();
    let rects = grid_layout(area, &grids);

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
        PanelType::Timeseries | PanelType::Gauge | PanelType::Stat => {
            match ChartModel::project(
                &panel.title,
                core_frame,
                &panel.thresholds,
                &ChartViewState::default(),
                usize::from(area.width),
            ) {
                Ok(model) => match panel.panel_type {
                    PanelType::Timeseries => draw_chart(frame, area, &model),
                    PanelType::Gauge => draw_gauge(frame, area, &model),
                    PanelType::Stat => draw_stat(frame, area, &model),
                    PanelType::Table => unreachable!("handled in the outer match arm"),
                },
                Err(err) => draw_placeholder(frame, area, &panel.title, &err.to_string()),
            }
        }
        PanelType::Table => {
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
