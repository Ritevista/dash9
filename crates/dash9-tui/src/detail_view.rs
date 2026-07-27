//! The focused panel's Space-toggled detail pane: combined config (query,
//! datasource, thresholds, grid position) and raw data (every row the
//! panel's query actually returned, not just what the small chart box can
//! show). Rendered in its own area **below** the main grid/layout/focus
//! area (`dash9/src/open.rs::draw_session`), never in place of it — the
//! chart(s) stay visible and usable the whole time a panel's detail is
//! open, so running a command that affects the focused panel (e.g.
//! `/panel threshold crit gte 95`) shows its effect here immediately,
//! since this redraws from live state every frame like everything else.
//!
//! Pure rendering, no I/O — same rule as every other module here.
//! `dash9-tui` can't depend on the binary crate's `LivePanel`/
//! `LiveSession`, so the composition root extracts these plain
//! fields from them each frame (mirrors `draw_panel`'s existing
//! split in `open.rs`).

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame as RatatuiFrame;

use dash9_core::{CommandError, Duration, Frame, PanelType, ValidatedThreshold};

use crate::draw::draw_table;
use crate::export::table_for_export;
use crate::pane::pane_block;
use crate::theme;

pub struct PanelDetail<'a> {
    pub title: &'a str,
    pub panel_type: PanelType,
    /// Pre-formatted by the caller (e.g. `"prom: prometheus
    /// http://localhost:9090"`) — building it needs cross-referencing
    /// the session's datasource map, which lives alongside
    /// `LivePanel` in the binary crate, not here.
    pub datasource_line: String,
    pub query: &'a str,
    pub allow_empty: bool,
    pub latency_budget: Option<Duration>,
    /// 1-indexed position among every panel on the dashboard (matches
    /// the zoom bar's own "panels X-Y of Z" numbering, `open.md`
    /// Section H) and the total panel count. Replaces showing the raw
    /// `GridSpec` — its `row`/`col` are internal layout units, not
    /// something a person can relate to anything else on screen (and
    /// can run into the thousands after a Grafana import rebases
    /// overlapping collapsed-row blocks apart, `docs/specs/
    /// grafana-dashboards.md` Section H) — reported live as
    /// "confusing." "Panel N of TOTAL" is the number a person actually
    /// wants: which one, out of how many.
    pub panel_number: usize,
    pub panel_count: usize,
    pub thresholds: &'a [ValidatedThreshold],
    pub last_result: Option<&'a Result<Frame, CommandError>>,
}

/// 6 fixed lines (title/type/datasource/query/panel/allow-empty) plus
/// either one "(none)" line, or a "Thresholds:" header *and* one line
/// per threshold — the header is a separate line from the entries it
/// introduces, not absorbed into the count of one of them. Plus the
/// block's own top/bottom border.
fn config_area_height(thresholds_len: usize) -> u16 {
    let config_lines = if thresholds_len == 0 {
        7
    } else {
        7 + thresholds_len
    };
    u16::try_from(config_lines + 2).unwrap_or(u16::MAX)
}

/// Rows given to the data table beyond the config block, when the caller
/// (`dash9/src/open.rs`) is sizing a *fixed-height* pane for this rather
/// than handing it the rest of the screen — e.g. the Space-toggled detail
/// pane, which sits below the main area and must leave room for the
/// output pane and command bar under it too.
const DEFAULT_DATA_ROWS: u16 = 8;

/// How tall a detail pane needs to be to show `thresholds_len`
/// thresholds' worth of config plus a reasonable data preview, capped at
/// `available`. Guarantees the config block (which `draw_panel_detail`
/// never truncates) always fits; the data table gets whatever's left,
/// same as it would if handed a taller area directly.
pub fn detail_height(thresholds_len: usize, available: u16) -> u16 {
    config_area_height(thresholds_len)
        .saturating_add(DEFAULT_DATA_ROWS)
        .min(available)
}

/// `None` when no panel is focused (an empty dashboard) — matches
/// the "no panel focused" placeholder every other panel-scoped
/// command already uses.
pub fn draw_panel_detail(frame: &mut RatatuiFrame, area: Rect, detail: Option<&PanelDetail>) {
    let Some(detail) = detail else {
        let block = Block::default().borders(Borders::ALL).title("detail");
        frame.render_widget(Paragraph::new("(no panel focused)").block(block), area);
        return;
    };

    let config_height = config_area_height(detail.thresholds.len());
    let [config_area, data_area] =
        Layout::vertical([Constraint::Length(config_height), Constraint::Min(0)]).areas(area);

    draw_config(frame, config_area, detail);
    draw_data(frame, data_area, detail);
}

fn draw_config(frame: &mut RatatuiFrame, area: Rect, detail: &PanelDetail) {
    let mut lines = vec![
        format!("Title: {}", detail.title),
        format!("Type: {}", detail.panel_type),
        format!("Datasource: {}", detail.datasource_line),
        format!("Query: {}", detail.query),
        format!("Panel: {} of {}", detail.panel_number, detail.panel_count),
        format!(
            "Allow empty: {} · Latency budget: {}",
            detail.allow_empty,
            detail
                .latency_budget
                .map_or_else(|| "(dashboard default)".to_string(), |d| d.to_string())
        ),
    ];
    if detail.thresholds.is_empty() {
        lines.push("Thresholds: (none)".to_string());
    } else {
        lines.push("Thresholds:".to_string());
        for t in detail.thresholds {
            lines.push(format!("  {} {} {}", t.name, t.op, t.value));
        }
    }

    let block = pane_block(
        &format!("detail: {}", detail.title),
        true,
        false,
        None,
        Some("Esc/space close"),
    );
    frame.render_widget(Paragraph::new(lines.join("\n")).block(block), area);
}

fn draw_data(frame: &mut RatatuiFrame, area: Rect, detail: &PanelDetail) {
    let Some(result) = detail.last_result else {
        draw_data_placeholder(frame, area, "(loading…)");
        return;
    };
    let core_frame = match result {
        Err(err) => return draw_data_placeholder(frame, area, &err.to_string()),
        Ok(core_frame) => core_frame,
    };
    if core_frame.is_empty() {
        return draw_data_placeholder(frame, area, "(no data)");
    }
    match table_for_export(core_frame) {
        Some(table) => draw_table(frame, area, &table, "data", false, false, false),
        None => draw_data_placeholder(frame, area, "(no table data)"),
    }
}

fn draw_data_placeholder(frame: &mut RatatuiFrame, area: Rect, message: &str) {
    let block = Block::default().borders(Borders::ALL).title("data");
    frame.render_widget(
        Paragraph::new(message.to_string())
            .style(Style::default().fg(theme::MUTED))
            .block(block),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash9_core::{DurationUnit, ErrorCode, FrameKind, FrameMeta, Point, Series, ThresholdOp};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::BTreeMap;

    fn backend(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).unwrap()
    }

    fn base_detail() -> PanelDetail<'static> {
        PanelDetail {
            title: "CPU Usage",
            panel_type: PanelType::Timeseries,
            datasource_line: "prom: prometheus http://localhost:9090".to_string(),
            query: "rate(node_cpu_seconds_total[5m])",
            allow_empty: false,
            latency_budget: Some(Duration {
                magnitude: 5,
                unit: DurationUnit::Seconds,
            }),
            panel_number: 1,
            panel_count: 4,
            thresholds: &[],
            last_result: None,
        }
    }

    fn data_frame() -> Frame {
        let mut labels = BTreeMap::new();
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

    #[test]
    fn no_panel_focused_draws_a_placeholder_without_panicking() {
        let mut terminal = backend(60, 20);
        terminal
            .draw(|f| draw_panel_detail(f, f.area(), None))
            .unwrap();
    }

    #[test]
    fn loading_placeholder_when_no_result_yet() {
        let detail = base_detail();
        let mut terminal = backend(60, 20);
        terminal
            .draw(|f| draw_panel_detail(f, f.area(), Some(&detail)))
            .unwrap();
    }

    #[test]
    fn error_placeholder_when_the_query_failed() {
        let mut detail = base_detail();
        let err = Err(CommandError::new(ErrorCode::E106, "boom", None));
        detail.last_result = Some(&err);
        let mut terminal = backend(60, 20);
        terminal
            .draw(|f| draw_panel_detail(f, f.area(), Some(&detail)))
            .unwrap();
    }

    #[test]
    fn draws_a_data_table_when_a_result_is_present() {
        let mut detail = base_detail();
        let frame = data_frame();
        let ok = Ok(frame);
        detail.last_result = Some(&ok);
        let mut terminal = backend(60, 20);
        terminal
            .draw(|f| draw_panel_detail(f, f.area(), Some(&detail)))
            .unwrap();
    }

    #[test]
    fn draws_thresholds_without_panicking() {
        let mut detail = base_detail();
        let thresholds = [ValidatedThreshold {
            name: "crit".to_string(),
            op: ThresholdOp::Gte,
            value: 95.0,
        }];
        detail.thresholds = &thresholds;
        let mut terminal = backend(60, 20);
        terminal
            .draw(|f| draw_panel_detail(f, f.area(), Some(&detail)))
            .unwrap();
    }

    /// Regression test for a real bug: the config box's height was
    /// computed one line short whenever thresholds were non-empty
    /// (the "Thresholds:" header line wasn't counted separately from
    /// the entries it introduces), so the last threshold silently
    /// clipped off the bottom of its own box instead of being shown.
    #[test]
    fn every_threshold_line_is_actually_visible_not_clipped() {
        let mut detail = base_detail();
        let thresholds = [
            ValidatedThreshold {
                name: "warn".to_string(),
                op: ThresholdOp::Gte,
                value: 0.75,
            },
            ValidatedThreshold {
                name: "crit".to_string(),
                op: ThresholdOp::Gte,
                value: 0.9,
            },
        ];
        detail.thresholds = &thresholds;
        let mut terminal = backend(60, 20);
        terminal
            .draw(|f| draw_panel_detail(f, f.area(), Some(&detail)))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(content.contains("warn"), "first threshold visible");
        assert!(
            content.contains("crit"),
            "second threshold must not be clipped off the bottom of the box"
        );
    }

    /// Regression test: the config block used to show the panel's raw
    /// `GridSpec` ("Grid: row 0, col 0, w 6, h 4") — internal layout
    /// units that mean nothing to a person and can run into the
    /// thousands after a Grafana import rebases collapsed-row blocks
    /// apart, reported live as confusing. `Panel: N of TOTAL` replaces
    /// it — the number a person actually wants: which panel, out of
    /// how many.
    #[test]
    fn detail_shows_panel_number_not_raw_grid_coordinates() {
        let mut detail = base_detail();
        detail.panel_number = 47;
        detail.panel_count = 124;
        let mut terminal = backend(60, 20);
        terminal
            .draw(|f| draw_panel_detail(f, f.area(), Some(&detail)))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            content.contains("Panel: 47 of 124"),
            "expected panel number, not raw grid coordinates: {content}"
        );
        assert!(!content.contains("Grid: row"));
    }

    #[test]
    fn zero_area_draws_without_panicking() {
        let detail = base_detail();
        let mut terminal = backend(60, 20);
        terminal
            .draw(|f| {
                draw_panel_detail(f, Rect::new(0, 0, 0, 0), Some(&detail));
            })
            .unwrap();
    }

    #[test]
    fn detail_height_grows_with_threshold_count_but_never_exceeds_available() {
        let no_thresholds = detail_height(0, 100);
        let two_thresholds = detail_height(2, 100);
        assert!(
            two_thresholds > no_thresholds,
            "more thresholds need a taller config block"
        );
        assert_eq!(detail_height(2, 5), 5, "never exceeds available space");
    }
}
