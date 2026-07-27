//! Ratatui draw code for [`ChartModel`]. Pure rendering (Mechanism
//! 5): no filesystem, network, process, or datasource access, and no
//! domain mutation. Everything this module needs already arrived
//! projected in the `ChartModel`; it only ever turns that data into
//! draw calls, choosing the deterministic text fallback on narrow
//! terminals or when there is nothing to plot.

use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::Style;
use ratatui::symbols::Marker;
use ratatui::widgets::{
    Axis, Chart, Dataset, Gauge, GraphType, Paragraph, Row, Table as RatatuiTable,
};
use ratatui::Frame;

use crate::chart::ChartModel;
use crate::pane::pane_block;
use crate::theme;

/// Below this width the deterministic text fallback renders instead
/// of the `Chart` widget — the "narrow terminal" responsive switch
/// Mechanism 3 requires.
const MIN_CHART_WIDTH: u16 = 40;

/// The one genuinely panel-specific action (`docs/specs/open.md`
/// Section G.2) — shown bottom-left, only on the focused panel, by
/// every panel-type draw function below. Broader navigation (Tab/`1`-`9`
/// selection, `PageUp`/`PageDown` paging, `+`/`-` zoom) is region-level,
/// not a property of any one panel, so it stays in the zoom bar instead
/// of being repeated on every panel's border.
pub const PANEL_HINT: &str = "space detail";

/// Draws one timeseries panel into `area`. Falls back to
/// [`ChartModel::render_text`] when the terminal is too narrow for an
/// axis/legend/braille chart to be legible, or when there are no
/// points to plot at all. `focused` drives the shared pane chrome
/// (`pane::pane_block`)'s border/name color — it stays true (and the
/// panel keeps looking targeted) even while the command box is
/// capturing keystrokes, since arrow keys/`Tab`/`Shift+Tab` still move
/// it then (`shell.rs::cycle_focused_panel`); `editing` is what tells
/// `pane_block` to render that continued focus via the calmer
/// `theme::FOCUS_DIM` instead of full-bright `theme::FOCUS` while it
/// isn't where keystrokes actually land (`pane_block`'s own docs).
/// `show_hint` is the separate, stricter "would `i` actually do
/// something right now" bit — `false` while editing, even on the
/// focused panel, since `i` is a literal character in the command box,
/// not the detail toggle, and a hint promising otherwise would be
/// actively misleading.
pub fn draw_chart(
    frame: &mut Frame,
    area: Rect,
    model: &ChartModel,
    focused: bool,
    editing: bool,
    show_hint: bool,
) {
    let has_points = model.series.iter().any(|s| !s.points.is_empty());
    if area.width < MIN_CHART_WIDTH || !has_points {
        draw_text_fallback(frame, area, model, focused, editing, show_hint);
        return;
    }

    let Some((x_min_ms, x_max_ms)) = x_bounds_ms(model) else {
        draw_text_fallback(frame, area, model, focused, editing, show_hint);
        return;
    };
    #[allow(clippy::cast_precision_loss)]
    let (x_min, x_max) = (x_min_ms as f64, x_max_ms as f64);

    let series_points: Vec<Vec<(f64, f64)>> = model
        .series
        .iter()
        .map(|s| {
            s.points
                .iter()
                .map(|p| (point_x(p.timestamp_ms), p.value))
                .collect()
        })
        .collect();
    let band_points: Vec<[(f64, f64); 2]> = model
        .thresholds
        .iter()
        .map(|band| [(x_min, band.value), (x_max, band.value)])
        .collect();

    let mut datasets = Vec::with_capacity(model.series.len() + model.thresholds.len());
    for (index, (series, points)) in model.series.iter().zip(&series_points).enumerate() {
        let mut style = Style::default().fg(theme::series_color(index));
        if series.highlighted {
            style = style.bold();
        }
        datasets.push(
            Dataset::default()
                .name(series.name.clone())
                .marker(Marker::Braille)
                .graph_type(GraphType::Line)
                .style(style)
                .data(points),
        );
    }
    for (band, points) in model.thresholds.iter().zip(&band_points) {
        datasets.push(
            Dataset::default()
                .name(format!("{} {} {:.2}", band.name, band.op, band.value))
                .marker(Marker::Dot)
                .graph_type(GraphType::Line)
                .style(Style::default().fg(theme::MUTED))
                .data(points),
        );
    }

    let x_midpoint_ms = x_min_ms + (x_max_ms - x_min_ms) / 2;
    let x_axis = Axis::default()
        .style(Style::default().fg(theme::TEXT))
        .bounds([x_min, x_max])
        .labels([
            relative_label(x_min_ms, x_max_ms),
            relative_label(x_midpoint_ms, x_max_ms),
            relative_label(x_max_ms, x_max_ms),
        ]);

    let y_min = model.y_min;
    let y_max = model.y_max;
    let y_midpoint = y_min + (y_max - y_min) / 2.0;
    let y_axis = Axis::default()
        .style(Style::default().fg(theme::TEXT))
        .bounds([y_min, y_max])
        .labels([
            format!("{y_min:.2}"),
            format!("{y_midpoint:.2}"),
            format!("{y_max:.2}"),
        ]);

    let block = pane_block(
        &model.title,
        focused,
        editing,
        status_for(model),
        show_hint.then_some(PANEL_HINT),
    );

    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(x_axis)
        .y_axis(y_axis);

    frame.render_widget(chart, area);
}

/// Layout zoom level's per-panel rendering (`docs/specs/session-layout.md`
/// Section A.1): title and border only, deliberately no chart/data at
/// all — Layout exists to confirm the dashboard's *arrangement*, and its
/// panels are routinely scaled down (`layout::grid_layout_fit`) well
/// below the size any chart/text-fallback rendering would be legible at,
/// so there is no "big enough" threshold to check here the way
/// `draw_chart`'s `MIN_CHART_WIDTH` has — every panel gets the same
/// outline treatment regardless of size. `i`/detail works in Layout same
/// as everywhere else (`ShellState::detail_open` is zoom-independent —
/// `docs/specs/session-layout.md` Section A.3's revision), so the
/// focused panel gets `PANEL_HINT` here too, same as the other
/// panel-type draw functions.
pub fn draw_panel_outline(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    focused: bool,
    editing: bool,
    show_hint: bool,
) {
    let block = pane_block(
        title,
        focused,
        editing,
        None,
        show_hint.then_some(PANEL_HINT),
    );
    frame.render_widget(block, area);
}

fn draw_text_fallback(
    frame: &mut Frame,
    area: Rect,
    model: &ChartModel,
    focused: bool,
    editing: bool,
    show_hint: bool,
) {
    let block = pane_block(
        &model.title,
        focused,
        editing,
        status_for(model),
        show_hint.then_some(PANEL_HINT),
    );
    // `render_text()`'s first line is the title, which the border above
    // already shows — trimmed here only, since `render_text()` itself is
    // shared with `dash9 test`'s CLI output and a bare title line there
    // is exactly right.
    let body = model.render_text();
    let body = body
        .strip_prefix(&model.title)
        .and_then(|rest| rest.strip_prefix('\n'))
        .unwrap_or(&body);
    let paragraph = Paragraph::new(body.to_string()).block(block);
    frame.render_widget(paragraph, area);
}

/// Draws a single-value "stat" panel: `model.current_value` as a big
/// centered number, with `model.current_severity`'s marker/label so
/// the state survives a monochrome terminal (Mechanism 4) — color is
/// a supplement, applied via `theme::severity_color`, never the only
/// carrier of meaning. The border status corner (top-right, same
/// `status_for` every panel type uses) does duplicate the body here —
/// deliberately: every panel type's chrome stays uniform (`docs/specs/open.md`
/// Section G.2) rather than some panels having one and others not, which
/// read as inconsistent/broken at a glance across a row of mixed panel
/// types.
pub fn draw_stat(
    frame: &mut Frame,
    area: Rect,
    model: &ChartModel,
    focused: bool,
    editing: bool,
    show_hint: bool,
) {
    let block = pane_block(
        &model.title,
        focused,
        editing,
        status_for(model),
        show_hint.then_some(PANEL_HINT),
    );
    let (text, style) = match (model.current_value, &model.current_severity) {
        (Some(value), Some(severity)) => (
            format!("{value:.3}\n{} {}", severity.marker(), severity.label()),
            Style::default().fg(theme::severity_color(severity)),
        ),
        _ => ("(no data)".to_string(), Style::default().fg(theme::MUTED)),
    };
    let paragraph = Paragraph::new(text)
        .style(style)
        .alignment(Alignment::Center)
        .block(block);
    frame.render_widget(paragraph, area);
}

/// Draws a single-value "gauge" panel as a percentage bar. SPEC.md
/// Section C.1 does not define a gauge min/max, so this assumes the
/// query result is already a 0-100 value — the only worked example
/// (`SPEC.md` C.2, "Disk Free %") is one. A documented v1
/// simplification, not a silent assumption; a future schema field
/// (e.g. `panels.gauge.max`) could relax this without breaking the
/// append-only grammar (SPEC.md B.1). The border status corner (see
/// `draw_stat`'s docs on why it's shown despite the bar being uniform
/// chrome) is the *only* place a gauge shows its severity marker/label
/// at all — the bar itself only carries severity as color, which isn't
/// monochrome-safe on its own (Mechanism 4); the border status fixes
/// that gap rather than just adding uniform styling.
pub fn draw_gauge(
    frame: &mut Frame,
    area: Rect,
    model: &ChartModel,
    focused: bool,
    editing: bool,
    show_hint: bool,
) {
    let block = pane_block(
        &model.title,
        focused,
        editing,
        status_for(model),
        show_hint.then_some(PANEL_HINT),
    );
    let Some(value) = model.current_value else {
        frame.render_widget(Paragraph::new("(no data)").block(block), area);
        return;
    };
    let ratio = (value / 100.0).clamp(0.0, 1.0);
    let color = model
        .current_severity
        .as_ref()
        .map_or(theme::SUCCESS, theme::severity_color);
    let gauge = Gauge::default()
        .block(block)
        .gauge_style(Style::default().fg(color))
        .ratio(ratio)
        .label(format!("{value:.1}%"));
    frame.render_widget(gauge, area);
}

/// Synthesizes a display [`dash9_core::Table`] from a
/// `Timeseries`/`InstantVector` frame's series — for when a panel is
/// declared `type = "table"` but its datasource has no native table
/// result shape. Prometheus's query API is exactly this case: it only
/// ever returns vector/matrix results, so `dash9-prom` never
/// constructs a `FrameKind::Table` frame (SPEC.md A.2's `Table` kind
/// has no v0.1 producer). Columns are every label key used by any
/// series (alphabetical — `Labels` is a `BTreeMap`, SPEC.md A.1) plus
/// a trailing `value` column from each series' latest point. Returns
/// `None` for an already-`Table`-kind frame (nothing to synthesize,
/// use its `table` field directly) or one with no series.
pub fn series_as_table(frame: &dash9_core::Frame) -> Option<dash9_core::Table> {
    use dash9_core::{ColumnKind, ColumnValues, FrameKind, Table, TableColumn};
    use std::collections::BTreeSet;

    if !matches!(frame.kind, FrameKind::Timeseries | FrameKind::InstantVector) {
        return None;
    }
    if frame.series.is_empty() {
        return None;
    }

    let mut label_keys: BTreeSet<&str> = BTreeSet::new();
    for series in &frame.series {
        label_keys.extend(series.labels.keys().map(String::as_str));
    }

    let mut columns: Vec<TableColumn> = label_keys
        .into_iter()
        .map(|key| TableColumn {
            name: key.to_string(),
            kind: ColumnKind::String,
            values: ColumnValues::String(
                frame
                    .series
                    .iter()
                    .map(|s| s.labels.get(key).cloned())
                    .collect(),
            ),
        })
        .collect();
    columns.push(TableColumn {
        name: "value".to_string(),
        kind: ColumnKind::Float,
        values: ColumnValues::Float(
            frame
                .series
                .iter()
                .map(|s| s.points.last().map(|p| p.value))
                .collect(),
        ),
    });

    Some(Table {
        row_count: frame.series.len(),
        columns,
    })
}

/// Draws a table panel directly from a `Table`-kind `Frame`'s
/// columns — bypasses `ChartModel`, which only projects
/// `Timeseries`/`InstantVector` frames (`ChartModel::project` rejects
/// a `Table` frame with `ChartError::UnsupportedFrameKind`; SPEC.md
/// A.2 defines `Table` as structurally different, column-oriented
/// data).
/// `focused` drives the shared pane chrome's border/name color, `editing`
/// whether that focus renders dimmed (`pane_block`'s docs), `show_hint`
/// whether the border also claims `i` will do something right now (`false`
/// while the command box is editing — see `draw_chart`'s docs for why the
/// two are separate). Pass `false, false, false` for the non-focus-ring
/// uses (e.g. `detail_view.rs`'s inner "data" sub-pane, which is never
/// itself a `Tab`-focusable target — it's already part of the focused
/// panel's own detail pane).
pub fn draw_table(
    frame: &mut Frame,
    area: Rect,
    table: &dash9_core::Table,
    title: &str,
    focused: bool,
    editing: bool,
    show_hint: bool,
) {
    let status = (table.row_count > 0).then(|| {
        let plural = if table.row_count == 1 { "" } else { "s" };
        (format!("{} row{plural}", table.row_count), theme::TEXT)
    });
    let block = pane_block(
        title,
        focused,
        editing,
        status,
        show_hint.then_some(PANEL_HINT),
    );
    if table.row_count == 0 {
        frame.render_widget(Paragraph::new("(no rows)").block(block), area);
        return;
    }

    let header = Row::new(table.columns.iter().map(|c| c.name.clone()))
        .style(Style::default().fg(theme::TEXT).bold());
    let rows: Vec<Row> = (0..table.row_count)
        .map(|row_index| Row::new(table.columns.iter().map(|c| column_cell(c, row_index))))
        .collect();
    let widths = vec![Constraint::Fill(1); table.columns.len()];

    let widget = RatatuiTable::new(rows, widths).header(header).block(block);
    frame.render_widget(widget, area);
}

pub(crate) fn column_cell(column: &dash9_core::TableColumn, row_index: usize) -> String {
    match &column.values {
        dash9_core::ColumnValues::Time(values) => values
            .get(row_index)
            .map(i64::to_string)
            .unwrap_or_default(),
        dash9_core::ColumnValues::Float(values) => values
            .get(row_index)
            .and_then(|v| *v)
            .map_or_else(|| "null".to_string(), |v| format!("{v:.3}")),
        dash9_core::ColumnValues::Int(values) => values
            .get(row_index)
            .and_then(|v| *v)
            .map_or_else(|| "null".to_string(), |v| v.to_string()),
        dash9_core::ColumnValues::String(values) => values
            .get(row_index)
            .and_then(Option::clone)
            .unwrap_or_else(|| "null".to_string()),
        dash9_core::ColumnValues::Bool(values) => values
            .get(row_index)
            .and_then(|v| *v)
            .map_or_else(|| "null".to_string(), |v| v.to_string()),
    }
}

#[allow(clippy::cast_precision_loss)]
fn point_x(timestamp_ms: i64) -> f64 {
    timestamp_ms as f64
}

fn x_bounds_ms(model: &ChartModel) -> Option<(i64, i64)> {
    let timestamps = model
        .series
        .iter()
        .flat_map(|s| s.points.iter().map(|p| p.timestamp_ms));
    let min = timestamps.clone().min();
    let max = timestamps.max();
    min.zip(max)
}

/// `ms` expressed as seconds-before `reference_ms`, e.g. `-30s`, or
/// `now` at zero offset. Relative rather than a calendar timestamp so
/// this needs no date-time dependency and stays meaningful whether
/// the underlying data is a minute old or a week old.
fn relative_label(ms: i64, reference_ms: i64) -> String {
    let delta_s = (reference_ms - ms) / 1000;
    if delta_s <= 0 {
        "now".to_string()
    } else {
        format!("-{delta_s}s")
    }
}

/// The pane border's top-right status corner (`pane::pane_block`) for a
/// chart/stat/gauge panel: the latest value plus severity marker/label
/// (Mechanism 4 — meaningful with color stripped, color is
/// `theme::severity_color` on top), or just the value with no threshold
/// breach to report, or nothing at all when there's no data yet (the
/// panel body already shows its own "(no data)"/"(loading…)" placeholder
/// — no need to repeat that in the border too).
fn status_for(model: &ChartModel) -> Option<(String, ratatui::style::Color)> {
    match (model.current_value, &model.current_severity) {
        (Some(value), Some(severity)) => Some((
            format!("{value:.3} {} {}", severity.marker(), severity.label()),
            theme::severity_color(severity),
        )),
        (Some(value), None) => Some((format!("{value:.3}"), theme::TEXT)),
        (None, _) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chart::ChartViewState;
    use dash9_core::{Frame as CoreFrame, FrameKind, FrameMeta, Point, Series};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::BTreeMap;

    fn labeled_series(job: &str, points: Vec<(i64, f64)>) -> Series {
        let mut labels = BTreeMap::new();
        labels.insert("job".to_string(), job.to_string());
        Series {
            labels,
            points: points
                .into_iter()
                .map(|(timestamp_ms, value)| Point {
                    timestamp_ms,
                    value,
                })
                .collect(),
        }
    }

    fn frame_with(points: Vec<(i64, f64)>) -> CoreFrame {
        CoreFrame {
            kind: FrameKind::Timeseries,
            series: vec![labeled_series("node", points)],
            table: None,
            meta: FrameMeta {
                query: "up".into(),
                datasource: "prom".into(),
                executed_at_ms: 0,
                warnings: vec![],
            },
        }
    }

    #[test]
    fn panel_outline_draws_without_panicking_focused_and_not() {
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_panel_outline(f, f.area(), "CPU Usage", true, false, true))
            .unwrap();
        terminal
            .draw(|f| draw_panel_outline(f, f.area(), "CPU Usage", false, false, false))
            .unwrap();
    }

    #[test]
    fn panel_outline_at_a_degenerate_tiny_size_draws_without_panicking() {
        let backend = TestBackend::new(3, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_panel_outline(f, f.area(), "CPU", false, false, false))
            .unwrap();
    }

    #[test]
    fn wide_area_with_data_draws_without_panicking() {
        let frame = frame_with(vec![(0, 1.0), (1000, 2.0), (2000, 1.5)]);
        let model =
            ChartModel::project("CPU", &frame, &[], &ChartViewState::default(), 80).unwrap();
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_chart(f, f.area(), &model, true, false, true))
            .unwrap();
    }

    #[test]
    fn narrow_area_falls_back_to_text_without_panicking() {
        let frame = frame_with(vec![(0, 1.0), (1000, 2.0)]);
        let model =
            ChartModel::project("CPU", &frame, &[], &ChartViewState::default(), 80).unwrap();
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_chart(f, f.area(), &model, false, false, false))
            .unwrap();
    }

    #[test]
    fn empty_model_falls_back_to_text_without_panicking() {
        let frame = frame_with(vec![]);
        let model =
            ChartModel::project("CPU", &frame, &[], &ChartViewState::default(), 80).unwrap();
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_chart(f, f.area(), &model, false, false, false))
            .unwrap();
    }

    #[test]
    fn relative_label_reports_now_at_zero_offset() {
        assert_eq!(relative_label(5000, 5000), "now");
        assert_eq!(relative_label(0, 5000), "-5s");
    }

    #[test]
    fn stat_draws_without_panicking_with_and_without_data() {
        let with_data = frame_with(vec![(0, 42.0)]);
        let model =
            ChartModel::project("Load", &with_data, &[], &ChartViewState::default(), 80).unwrap();
        let backend = TestBackend::new(20, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_stat(f, f.area(), &model, true, false, true))
            .unwrap();

        let empty = frame_with(vec![]);
        let empty_model =
            ChartModel::project("Load", &empty, &[], &ChartViewState::default(), 80).unwrap();
        terminal
            .draw(|f| draw_stat(f, f.area(), &empty_model, false, false, false))
            .unwrap();
    }

    #[test]
    fn gauge_draws_without_panicking_with_and_without_data() {
        let with_data = frame_with(vec![(0, 72.5)]);
        let model =
            ChartModel::project("Disk", &with_data, &[], &ChartViewState::default(), 80).unwrap();
        let backend = TestBackend::new(30, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_gauge(f, f.area(), &model, true, false, true))
            .unwrap();

        let empty = frame_with(vec![]);
        let empty_model =
            ChartModel::project("Disk", &empty, &[], &ChartViewState::default(), 80).unwrap();
        terminal
            .draw(|f| draw_gauge(f, f.area(), &empty_model, false, false, false))
            .unwrap();
    }

    /// Regression test: `draw_stat`/`draw_gauge` used to pass `None` for
    /// the border status corner, on the theory that the body already
    /// shows severity — but `draw_gauge`'s body never showed a
    /// marker/label at all (only bar color, not monochrome-safe), and
    /// having some panel types show border status and others not read as
    /// inconsistent chrome across a row of mixed panel types
    /// (`docs/specs/open.md` Section G.2). Both must now show it.
    #[test]
    fn stat_and_gauge_show_the_severity_marker_in_the_border_status_corner() {
        use dash9_core::{ThresholdOp, ValidatedThreshold};

        let thresholds = [ValidatedThreshold {
            name: "warn".to_string(),
            op: ThresholdOp::Gte,
            value: 0.0, // breached by any non-negative value below
        }];

        let frame = frame_with(vec![(0, 42.0)]);
        let stat_model =
            ChartModel::project("Load", &frame, &thresholds, &ChartViewState::default(), 80)
                .unwrap();
        let backend = TestBackend::new(30, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_stat(f, f.area(), &stat_model, true, false, true))
            .unwrap();
        let stat_content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            stat_content.contains("warn"),
            "stat panel border must show the breached threshold's label: {stat_content}"
        );

        let gauge_model =
            ChartModel::project("Disk", &frame, &thresholds, &ChartViewState::default(), 80)
                .unwrap();
        terminal
            .draw(|f| draw_gauge(f, f.area(), &gauge_model, true, false, true))
            .unwrap();
        let gauge_content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            gauge_content.contains("warn"),
            "gauge panel border must show the breached threshold's label \
             — otherwise severity is color-only, not monochrome-safe: {gauge_content}"
        );
    }

    #[test]
    fn series_as_table_builds_label_columns_plus_a_value_column() {
        let mut a_labels = BTreeMap::new();
        a_labels.insert("instance".to_string(), "10.0.0.1:9100".to_string());
        let mut b_labels = BTreeMap::new();
        b_labels.insert("instance".to_string(), "10.0.0.2:9100".to_string());

        let frame = CoreFrame {
            kind: FrameKind::InstantVector,
            series: vec![
                Series {
                    labels: a_labels,
                    points: vec![Point {
                        timestamp_ms: 0,
                        value: 0.47,
                    }],
                },
                Series {
                    labels: b_labels,
                    points: vec![Point {
                        timestamp_ms: 0,
                        value: 0.13,
                    }],
                },
            ],
            table: None,
            meta: FrameMeta {
                query: "node_load1".into(),
                datasource: "prom".into(),
                executed_at_ms: 0,
                warnings: vec![],
            },
        };

        let table = series_as_table(&frame).unwrap();
        assert_eq!(table.row_count, 2);
        assert_eq!(table.columns.len(), 2);
        assert_eq!(table.columns[0].name, "instance");
        assert_eq!(table.columns[1].name, "value");
    }

    #[test]
    fn series_as_table_returns_none_for_a_table_kind_frame_or_no_series() {
        let table_frame = CoreFrame {
            kind: FrameKind::Table,
            series: vec![],
            table: None,
            meta: FrameMeta {
                query: "up".into(),
                datasource: "prom".into(),
                executed_at_ms: 0,
                warnings: vec![],
            },
        };
        assert!(series_as_table(&table_frame).is_none());

        let empty_frame = frame_with(vec![]);
        let empty_instant = CoreFrame {
            series: vec![],
            ..empty_frame
        };
        assert!(series_as_table(&empty_instant).is_none());
    }

    #[test]
    fn table_draws_without_panicking_with_and_without_rows() {
        use dash9_core::{ColumnKind, ColumnValues, Table, TableColumn};

        let table = Table {
            columns: vec![
                TableColumn {
                    name: "instance".to_string(),
                    kind: ColumnKind::String,
                    values: ColumnValues::String(vec![Some("10.0.0.1:9100".to_string()), None]),
                },
                TableColumn {
                    name: "load1".to_string(),
                    kind: ColumnKind::Float,
                    values: ColumnValues::Float(vec![Some(0.47), None]),
                },
            ],
            row_count: 2,
        };
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_table(f, f.area(), &table, "Top Processes", true, false, true))
            .unwrap();

        let empty_table = Table {
            columns: vec![],
            row_count: 0,
        };
        terminal
            .draw(|f| {
                draw_table(
                    f,
                    f.area(),
                    &empty_table,
                    "Top Processes",
                    false,
                    false,
                    false,
                );
            })
            .unwrap();
    }
}
