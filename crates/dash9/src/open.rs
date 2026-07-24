//! `dash9 open` v1: a live, read-only multi-panel viewer. Loads a
//! dashboard TOML, lays out its panels on the 12-column grid (SPEC.md
//! C.1), polls each panel's datasource on `dashboard.refresh`'s
//! cadence, and renders all four panel types with real data.
//!
//! The command bar and runtime editing (`ds add`, `panel
//! type`/`threshold`/`title`, `range`, `refresh`, `dash save`, ad-hoc
//! `q` — SPEC.md Section B) are deliberately not part of this slice;
//! `dash9_core::command::parse` is fully built and tested, but wiring
//! it to a command-bar widget and mutating live session state is a
//! separable follow-up. `focused_panel` below exists for that
//! follow-up to consume — in v1 it is cosmetic (border color only).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use dash9_core::{
    load_path, validate, CommandError, Frame as CoreFrame, PanelType, RefreshInterval,
    ValidatedDashboard, ValidatedPanel,
};
use dash9_prom::PrometheusDatasource;
use dash9_tui::chart::{ChartModel, ChartViewState};
use dash9_tui::{
    draw_chart, draw_gauge, draw_stat, draw_table, grid_layout, series_as_table, theme,
};
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tokio::sync::mpsc;

use crate::datasources::{build_datasources, epoch_ms_now, execute_panel_query};

const TICK: StdDuration = StdDuration::from_millis(250);
const CHANNEL_CAPACITY: usize = 64;

/// One panel's latest fetch outcome, delivered from its polling task
/// to the render loop. `elapsed_ms` is captured here (composition
/// root owns timing, same as `dash9 test`) even though v1's draw code
/// doesn't surface it yet.
struct PanelUpdate {
    panel_index: usize,
    result: Result<CoreFrame, CommandError>,
    #[allow(dead_code)]
    elapsed_ms: i64,
}

pub fn run(path: &Path) -> anyhow::Result<()> {
    let dashboard = load_path(path)
        .and_then(|file| validate(&file))
        .map_err(|err| anyhow::anyhow!("dashboard invalid: {err}"))?;
    let dashboard = Arc::new(dashboard);

    let datasources: HashMap<String, Arc<PrometheusDatasource>> = build_datasources(&dashboard)
        .into_iter()
        .map(|(name, ds)| (name, Arc::new(ds)))
        .collect();

    let (tx, mut rx) = mpsc::channel::<PanelUpdate>(CHANNEL_CAPACITY);
    for panel_index in 0..dashboard.panels.len() {
        let panel = &dashboard.panels[panel_index];
        let Some(datasource) = datasources.get(&panel.datasource) else {
            // `validate()` already guarantees every panel's
            // `datasource` matches a configured entry (same invariant
            // `dash9 test` relies on).
            anyhow::bail!(
                "internal error: panel \"{}\" references unconfigured datasource \"{}\"",
                panel.title,
                panel.datasource
            );
        };
        spawn_panel_poller(
            panel_index,
            Arc::clone(datasource),
            Arc::clone(&dashboard),
            tx.clone(),
        );
    }
    drop(tx);

    let mut panel_state: Vec<Option<Result<CoreFrame, CommandError>>> =
        vec![None; dashboard.panels.len()];
    let mut focused_panel = 0usize;

    ratatui::run(|terminal| -> anyhow::Result<()> {
        loop {
            while let Ok(update) = rx.try_recv() {
                panel_state[update.panel_index] = Some(update.result);
            }

            terminal.draw(|f| {
                draw_dashboard(f, f.area(), &dashboard, &panel_state, focused_panel);
            })?;

            if event::poll(TICK)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Tab if !dashboard.panels.is_empty() => {
                                focused_panel = (focused_panel + 1) % dashboard.panels.len();
                            }
                            KeyCode::BackTab if !dashboard.panels.is_empty() => {
                                let len = dashboard.panels.len();
                                focused_panel = (focused_panel + len - 1) % len;
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    })
}

/// One polling task per panel: fetches on `dashboard.refresh`'s
/// cadence and sends every outcome (success or failure) over `tx`.
/// `RefreshInterval::Off` means fetch once and stop, matching the
/// verb's SPEC.md B.3 semantics ("off" disables auto-refresh).
fn spawn_panel_poller(
    panel_index: usize,
    datasource: Arc<PrometheusDatasource>,
    dashboard: Arc<ValidatedDashboard>,
    tx: mpsc::Sender<PanelUpdate>,
) {
    tokio::spawn(async move {
        let panel = &dashboard.panels[panel_index];
        let refresh_duration = match dashboard.refresh {
            RefreshInterval::Duration(d) => Some(StdDuration::from_millis(
                u64::try_from(d.as_millis()).unwrap_or(u64::MAX),
            )),
            RefreshInterval::Off => None,
        };

        loop {
            let now_ms = epoch_ms_now();
            let started = Instant::now();
            let result = execute_panel_query(&datasource, panel, &dashboard, now_ms).await;
            let elapsed_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);

            let update = PanelUpdate {
                panel_index,
                result,
                elapsed_ms,
            };
            if tx.send(update).await.is_err() {
                return; // Render loop exited; stop polling.
            }

            match refresh_duration {
                Some(interval) => tokio::time::sleep(interval).await,
                None => return,
            }
        }
    });
}

fn draw_dashboard(
    frame: &mut Frame,
    area: Rect,
    dashboard: &ValidatedDashboard,
    panel_state: &[Option<Result<CoreFrame, CommandError>>],
    focused_panel: usize,
) {
    let grids: Vec<_> = dashboard.panels.iter().map(|p| p.grid).collect();
    let rects = grid_layout(area, &grids);

    for (index, panel) in dashboard.panels.iter().enumerate() {
        let rect = rects[index];
        if rect.width == 0 || rect.height == 0 {
            continue; // Positioned entirely off-screen (v1 has no scrolling).
        }
        draw_panel(frame, rect, panel, panel_state[index].as_ref());
        recolor_border(frame, rect, index == focused_panel);
    }
}

fn draw_panel(
    frame: &mut Frame,
    area: Rect,
    panel: &ValidatedPanel,
    state: Option<&Result<CoreFrame, CommandError>>,
) {
    let Some(result) = state else {
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
/// a v1 feature that's cosmetic only (see module docs).
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
