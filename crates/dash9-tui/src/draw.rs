//! Ratatui draw code for [`ChartModel`]. Pure rendering (Mechanism
//! 5): no filesystem, network, process, or datasource access, and no
//! domain mutation. Everything this module needs already arrived
//! projected in the `ChartModel`; it only ever turns that data into
//! draw calls, choosing the deterministic text fallback on narrow
//! terminals or when there is nothing to plot.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::symbols::Marker;
use ratatui::widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph};
use ratatui::Frame;

use crate::chart::ChartModel;
use crate::theme;

/// Below this width the deterministic text fallback renders instead
/// of the `Chart` widget — the "narrow terminal" responsive switch
/// Mechanism 3 requires.
const MIN_CHART_WIDTH: u16 = 40;

/// Draws one timeseries panel into `area`. Falls back to
/// [`ChartModel::render_text`] when the terminal is too narrow for an
/// axis/legend/braille chart to be legible, or when there are no
/// points to plot at all.
pub fn draw_chart(frame: &mut Frame, area: Rect, model: &ChartModel) {
    let has_points = model.series.iter().any(|s| !s.points.is_empty());
    if area.width < MIN_CHART_WIDTH || !has_points {
        draw_text_fallback(frame, area, model);
        return;
    }

    let Some((x_min_ms, x_max_ms)) = x_bounds_ms(model) else {
        draw_text_fallback(frame, area, model);
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

    let block = Block::default()
        .borders(Borders::ALL)
        .title(model.title.clone())
        .title_bottom(current_status_line(model));

    let chart = Chart::new(datasets)
        .block(block)
        .x_axis(x_axis)
        .y_axis(y_axis);

    frame.render_widget(chart, area);
}

fn draw_text_fallback(frame: &mut Frame, area: Rect, model: &ChartModel) {
    let paragraph =
        Paragraph::new(model.render_text()).block(Block::default().borders(Borders::ALL));
    frame.render_widget(paragraph, area);
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

fn current_status_line(model: &ChartModel) -> String {
    match (model.current_value, &model.current_severity) {
        (Some(value), Some(severity)) => {
            format!(
                "current: {value:.3} [{} {}]",
                severity.marker(),
                severity.label()
            )
        }
        _ => "current: (no data)".to_string(),
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
    fn wide_area_with_data_draws_without_panicking() {
        let frame = frame_with(vec![(0, 1.0), (1000, 2.0), (2000, 1.5)]);
        let model =
            ChartModel::project("CPU", &frame, &[], &ChartViewState::default(), 80).unwrap();
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_chart(f, f.area(), &model)).unwrap();
    }

    #[test]
    fn narrow_area_falls_back_to_text_without_panicking() {
        let frame = frame_with(vec![(0, 1.0), (1000, 2.0)]);
        let model =
            ChartModel::project("CPU", &frame, &[], &ChartViewState::default(), 80).unwrap();
        let backend = TestBackend::new(20, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_chart(f, f.area(), &model)).unwrap();
    }

    #[test]
    fn empty_model_falls_back_to_text_without_panicking() {
        let frame = frame_with(vec![]);
        let model =
            ChartModel::project("CPU", &frame, &[], &ChartViewState::default(), 80).unwrap();
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_chart(f, f.area(), &model)).unwrap();
    }

    #[test]
    fn relative_label_reports_now_at_zero_offset() {
        assert_eq!(relative_label(5000, 5000), "now");
        assert_eq!(relative_label(0, 5000), "-5s");
    }
}
