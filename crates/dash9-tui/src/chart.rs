//! Timeseries chart projection: `Frame` -> `ChartModel`.
//!
//! This module owns no Ratatui types, colors, or terminal state (dash9
//! rendering Mechanism 1). It is constructible and assertable in a
//! test with no terminal. Widget-facing color/marker mapping for
//! [`Severity`] lives in `theme::severity_color`; `Severity::marker`
//! and `Severity::label` here are the non-color-dependent shape/text
//! carriers (Mechanism 4).

use std::fmt::Write as _;

use dash9_core::{Frame, FrameKind, Point, ThresholdOp, ValidatedThreshold};

/// A chart never draws more series than this; beyond it, series are
/// ranked by latest value and the rest are dropped with the count
/// surfaced in [`ChartModel::truncated_series_count`] rather than
/// silently disappearing.
pub(crate) const MAX_DISPLAYED_SERIES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartError {
    /// Only `Timeseries` and `InstantVector` frames project onto a
    /// chart; a `Table` frame has no series to project.
    UnsupportedFrameKind(FrameKind),
    /// `view.zoom` had `start_ms > end_ms`.
    InvalidZoomRange { start_ms: i64, end_ms: i64 },
}

impl std::fmt::Display for ChartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChartError::UnsupportedFrameKind(kind) => {
                write!(f, "frame kind {kind:?} cannot be projected onto a chart")
            }
            ChartError::InvalidZoomRange { start_ms, end_ms } => {
                write!(
                    f,
                    "zoom range start_ms ({start_ms}) is after end_ms ({end_ms})"
                )
            }
        }
    }
}

impl std::error::Error for ChartError {}

/// Interactive state that shapes a projection but must never leak into
/// a `Frame` or into anything exported/saved (Mechanism 2). Discarding
/// and rebuilding this alongside a fresh `Frame` is expected — a
/// `selected_series` index that no longer exists after series churn is
/// treated as "no selection", not a validation error.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ChartViewState {
    /// Restrict the chart to `[start_ms, end_ms]`, inclusive. `None`
    /// shows the full range in the `Frame`.
    pub zoom: Option<(i64, i64)>,
    /// Index into `Frame::series` of the series to highlight and to
    /// prefer when picking the "current value" reading.
    pub selected_series: Option<usize>,
}

/// One configured threshold, carried through as plain data (name, op,
/// value) with no rendering decision attached.
#[derive(Debug, Clone, PartialEq)]
pub struct ThresholdBand {
    pub name: String,
    pub op: ThresholdOp,
    pub value: f64,
}

impl From<&ValidatedThreshold> for ThresholdBand {
    fn from(t: &ValidatedThreshold) -> Self {
        ThresholdBand {
            name: t.name.clone(),
            op: t.op,
            value: t.value,
        }
    }
}

impl ThresholdBand {
    fn fires(&self, value: f64) -> bool {
        match self.op {
            ThresholdOp::Gt => value > self.value,
            ThresholdOp::Gte => value >= self.value,
            ThresholdOp::Lt => value < self.value,
            ThresholdOp::Lte => value <= self.value,
        }
    }
}

/// The semantic status of a scalar value against a panel's configured
/// thresholds. Never a color: [`Severity::marker`] and
/// [`Severity::label`] carry the meaning on their own.
#[derive(Debug, Clone, PartialEq)]
pub enum Severity {
    Ok,
    /// The most severely breached band among every band whose
    /// predicate fired, ranked by how extreme the *band's own
    /// configured value* is in its breach direction (highest `value`
    /// for `gt`/`gte`, lowest `value` for `lt`/`lte`) — the band that
    /// is hardest to cross wins. This ranks breaches without assuming
    /// any convention about threshold names (dashboards may name
    /// bands anything).
    Breached(ThresholdBand),
}

impl Severity {
    pub fn evaluate(value: f64, bands: &[ThresholdBand]) -> Severity {
        fn severity_rank(band: &ThresholdBand) -> f64 {
            match band.op {
                ThresholdOp::Gt | ThresholdOp::Gte => band.value,
                ThresholdOp::Lt | ThresholdOp::Lte => -band.value,
            }
        }
        bands
            .iter()
            .filter(|band| band.fires(value))
            .max_by(|a, b| severity_rank(a).total_cmp(&severity_rank(b)))
            .cloned()
            .map_or(Severity::Ok, Severity::Breached)
    }

    /// Shape carrying the meaning regardless of color (Mechanism 4).
    pub fn marker(&self) -> char {
        match self {
            Severity::Ok => '●',
            Severity::Breached(_) => '▲',
        }
    }

    /// Text carrying the meaning regardless of color (Mechanism 4).
    pub fn label(&self) -> String {
        match self {
            Severity::Ok => "ok".to_string(),
            Severity::Breached(band) => band.name.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChartSeries {
    pub name: String,
    /// Downsampled to at most the requested width, in ascending
    /// timestamp order. Empty if the series matched no points in the
    /// zoomed range.
    pub points: Vec<Point>,
    pub highlighted: bool,
}

/// A presentation-agnostic projection of a `Frame` for the timeseries
/// chart renderer. Stores data, labels, thresholds, and semantic
/// status only — see module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct ChartModel {
    pub title: String,
    pub series: Vec<ChartSeries>,
    pub thresholds: Vec<ThresholdBand>,
    pub y_min: f64,
    pub y_max: f64,
    /// The latest value of the "primary" series (the highlighted
    /// series if one is selected and present, else the first series),
    /// taken before downsampling so it is never an average.
    pub current_value: Option<f64>,
    pub current_severity: Option<Severity>,
    /// How many series beyond the chart's display cap were ranked out
    /// of the projection. Zero when nothing was dropped.
    pub truncated_series_count: usize,
    /// The highest latest-value across *every* series the query returned
    /// — computed before the display cap (`MAX_DISPLAYED_SERIES`) drops
    /// any, so it reflects the real query result, not just what's drawn.
    /// `None` when there are no series at all. Used by
    /// `draw_gauge`/`dash9_core::ValidatedPanel::gauge_max`'s `None`
    /// ("auto") case — a bargauge/gauge panel with no fixed ceiling in
    /// its Grafana export auto-scales against whichever of its own
    /// series currently has the highest value, matching real Grafana
    /// behavior (`docs/specs/grafana-dashboards.md` Section H).
    pub max_across_series: Option<f64>,
}

impl ChartModel {
    pub fn project(
        title: impl Into<String>,
        frame: &Frame,
        thresholds: &[ValidatedThreshold],
        view: &ChartViewState,
        width: usize,
    ) -> Result<ChartModel, ChartError> {
        if !matches!(frame.kind, FrameKind::Timeseries | FrameKind::InstantVector) {
            return Err(ChartError::UnsupportedFrameKind(frame.kind));
        }
        if let Some((start_ms, end_ms)) = view.zoom {
            if start_ms > end_ms {
                return Err(ChartError::InvalidZoomRange { start_ms, end_ms });
            }
        }

        let bands: Vec<ThresholdBand> = thresholds.iter().map(ThresholdBand::from).collect();
        let width = width.max(1);

        let zoomed: Vec<Vec<Point>> = frame
            .series
            .iter()
            .map(|s| zoom_filter(&s.points, view.zoom))
            .collect();

        let selected = view
            .selected_series
            .filter(|&index| index < frame.series.len());

        let keep = select_series(&zoomed, selected);
        let truncated_series_count = frame.series.len() - keep.len();

        let series: Vec<ChartSeries> = keep
            .iter()
            .map(|&index| ChartSeries {
                name: series_name(&frame.series[index].labels, index),
                points: downsample(&zoomed[index], width),
                highlighted: selected == Some(index),
            })
            .collect();

        let primary_index = selected.or(if frame.series.is_empty() {
            None
        } else {
            Some(0)
        });
        let current_value = primary_index
            .and_then(|index| last_point(&zoomed[index]))
            .map(|p| p.value);
        let current_severity = current_value.map(|value| Severity::evaluate(value, &bands));

        let (y_min, y_max) = axis_bounds(&series, &bands);

        let max_across_series = {
            let max = zoomed
                .iter()
                .filter_map(|points| last_point(points))
                .map(|p| p.value)
                .fold(f64::NEG_INFINITY, f64::max);
            max.is_finite().then_some(max)
        };

        Ok(ChartModel {
            title: title.into(),
            series,
            thresholds: bands,
            y_min,
            y_max,
            current_value,
            current_severity,
            truncated_series_count,
            max_across_series,
        })
    }

    /// Deterministic, Ratatui-free compact renderer used for narrow
    /// terminals, `dash9 test` output, and report/export paths
    /// (Mechanism 3). Stable series/threshold ordering and fixed
    /// float formatting keep it reproducible across CI runs and VHS
    /// recordings; timestamps are formatted in UTC for the same
    /// reason (see docs/architecture/rendering.md divergences).
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{}", self.title);

        if self.series.is_empty() {
            out.push_str("(no data)\n");
        }

        for series in &self.series {
            let marker = if series.highlighted { "*" } else { " " };
            let _ = writeln!(out, "{marker}{}", series.name);
            if series.points.is_empty() {
                out.push_str("  (no points in range)\n");
                continue;
            }
            let _ = writeln!(out, "  {}", sparkline(&series.points));
            let min = series
                .points
                .iter()
                .map(|p| p.value)
                .fold(f64::INFINITY, f64::min);
            let max = series
                .points
                .iter()
                .map(|p| p.value)
                .fold(f64::NEG_INFINITY, f64::max);
            let _ = writeln!(
                out,
                "  min {min:.3}  max {max:.3}  n={}",
                series.points.len()
            );
        }

        if self.truncated_series_count > 0 {
            let _ = writeln!(out, "... {} series not shown", self.truncated_series_count);
        }

        if !self.thresholds.is_empty() {
            out.push_str("thresholds:\n");
            for band in &self.thresholds {
                let _ = writeln!(out, "  {} {} {:.3}", band.name, band.op, band.value);
            }
        }

        if let (Some(value), Some(severity)) = (self.current_value, &self.current_severity) {
            let _ = writeln!(
                out,
                "current: {value:.3} [{} {}]",
                severity.marker(),
                severity.label()
            );
        }

        out
    }
}

fn zoom_filter(points: &[Point], zoom: Option<(i64, i64)>) -> Vec<Point> {
    let mut filtered: Vec<Point> = match zoom {
        Some((start_ms, end_ms)) => points
            .iter()
            .copied()
            .filter(|p| p.timestamp_ms >= start_ms && p.timestamp_ms <= end_ms)
            .collect(),
        None => points.to_vec(),
    };
    filtered.sort_by_key(|p| p.timestamp_ms);
    filtered
}

fn last_point(points: &[Point]) -> Option<Point> {
    points.iter().max_by_key(|p| p.timestamp_ms).copied()
}

/// Ranks series by latest value (descending, missing treated as
/// lowest) and keeps at most [`MAX_DISPLAYED_SERIES`], always
/// including `selected` if present. Returns kept indices in ascending
/// (original `Frame` order) so display order stays stable regardless
/// of ranking.
fn select_series(zoomed: &[Vec<Point>], selected: Option<usize>) -> Vec<usize> {
    if zoomed.len() <= MAX_DISPLAYED_SERIES {
        return (0..zoomed.len()).collect();
    }
    let mut ranked: Vec<usize> = (0..zoomed.len()).collect();
    ranked.sort_by(|&a, &b| {
        let va = last_point(&zoomed[a]).map_or(f64::NEG_INFINITY, |p| p.value);
        let vb = last_point(&zoomed[b]).map_or(f64::NEG_INFINITY, |p| p.value);
        vb.total_cmp(&va)
    });
    let mut keep: Vec<usize> = ranked.into_iter().take(MAX_DISPLAYED_SERIES).collect();
    if let Some(sel) = selected {
        if !keep.contains(&sel) {
            keep.pop();
            keep.push(sel);
        }
    }
    keep.sort_unstable();
    keep
}

fn series_name(labels: &dash9_core::Labels, index: usize) -> String {
    if labels.is_empty() {
        format!("series {}", index + 1)
    } else {
        labels
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// Buckets `points` (assumed already time-sorted) into at most `width`
/// averaged samples. A no-op when there are already `width` or fewer
/// points; never synthesizes points for empty input (SPEC.md A.2's
/// "no forced alignment" philosophy extends to the projection).
///
/// Bucket-assignment arithmetic only needs to place a point in
/// roughly the right terminal column, not preserve exact timestamp
/// precision, so the `i64`/`f64`/`usize` conversions below are
/// intentionally lossy.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn downsample(points: &[Point], width: usize) -> Vec<Point> {
    if points.len() <= width {
        return points.to_vec();
    }
    let min_ts = points[0].timestamp_ms;
    let max_ts = points[points.len() - 1].timestamp_ms;
    let span = (max_ts - min_ts).max(1) as f64;

    let mut buckets: Vec<Vec<Point>> = vec![Vec::new(); width];
    for p in points {
        let frac = (p.timestamp_ms - min_ts) as f64 / span;
        let idx = ((frac * width as f64) as usize).min(width - 1);
        buckets[idx].push(*p);
    }

    buckets
        .into_iter()
        .filter(|bucket| !bucket.is_empty())
        .map(|bucket| {
            let n = bucket.len() as f64;
            let timestamp_ms =
                (bucket.iter().map(|p| p.timestamp_ms).sum::<i64>() as f64 / n).round() as i64;
            let value = bucket.iter().map(|p| p.value).sum::<f64>() / n;
            Point {
                timestamp_ms,
                value,
            }
        })
        .collect()
}

fn axis_bounds(series: &[ChartSeries], bands: &[ThresholdBand]) -> (f64, f64) {
    let values = series
        .iter()
        .flat_map(|s| s.points.iter().map(|p| p.value))
        .chain(bands.iter().map(|b| b.value));
    let min = values.clone().fold(f64::INFINITY, f64::min);
    let max = values.fold(f64::NEG_INFINITY, f64::max);
    if min.is_finite() && max.is_finite() {
        (min, max)
    } else {
        (0.0, 1.0)
    }
}

const SPARKLINE_LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// Deterministic ASCII/Unicode sparkline: no color, no Ratatui,
/// reproducible from `points` alone. The bucket-level math has the
/// same lossy-conversion tradeoff as [`downsample`].
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn sparkline(points: &[Point]) -> String {
    let min = points.iter().map(|p| p.value).fold(f64::INFINITY, f64::min);
    let max = points
        .iter()
        .map(|p| p.value)
        .fold(f64::NEG_INFINITY, f64::max);
    let span = max - min;
    points
        .iter()
        .map(|p| {
            let level = if span <= 0.0 {
                3
            } else {
                (((p.value - min) / span) * (SPARKLINE_LEVELS.len() - 1) as f64).round() as usize
            };
            SPARKLINE_LEVELS[level.min(SPARKLINE_LEVELS.len() - 1)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash9_core::{FrameMeta, Series};
    use std::collections::BTreeMap;

    fn frame(kind: FrameKind, series: Vec<Series>) -> Frame {
        Frame {
            kind,
            series,
            table: None,
            meta: FrameMeta {
                query: "up".into(),
                datasource: "prom".into(),
                executed_at_ms: 0,
                warnings: vec![],
            },
        }
    }

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

    #[test]
    fn constructible_and_assertable_with_no_terminal() {
        let f = frame(
            FrameKind::Timeseries,
            vec![labeled_series("node", vec![(0, 1.0), (1000, 2.0)])],
        );
        let model = ChartModel::project("CPU", &f, &[], &ChartViewState::default(), 80).unwrap();
        assert_eq!(model.title, "CPU");
        assert_eq!(model.series.len(), 1);
        assert_eq!(model.series[0].name, "job=node");
        assert_eq!(model.current_value, Some(2.0));
    }

    #[test]
    fn table_frame_is_rejected() {
        let f = frame(FrameKind::Table, vec![]);
        let err = ChartModel::project("t", &f, &[], &ChartViewState::default(), 80).unwrap_err();
        assert_eq!(err, ChartError::UnsupportedFrameKind(FrameKind::Table));
    }

    #[test]
    fn invalid_zoom_range_is_rejected() {
        let f = frame(
            FrameKind::Timeseries,
            vec![labeled_series("node", vec![(0, 1.0)])],
        );
        let view = ChartViewState {
            zoom: Some((100, 0)),
            selected_series: None,
        };
        let err = ChartModel::project("t", &f, &[], &view, 80).unwrap_err();
        assert_eq!(
            err,
            ChartError::InvalidZoomRange {
                start_ms: 100,
                end_ms: 0
            }
        );
    }

    #[test]
    fn empty_frame_projects_to_empty_model_not_an_error() {
        let f = frame(FrameKind::Timeseries, vec![]);
        let model = ChartModel::project("t", &f, &[], &ChartViewState::default(), 80).unwrap();
        assert!(model.series.is_empty());
        assert_eq!(model.current_value, None);
        assert!(model.render_text().contains("(no data)"));
    }

    #[test]
    fn zoom_filters_points_outside_range() {
        let f = frame(
            FrameKind::Timeseries,
            vec![labeled_series(
                "node",
                vec![(0, 1.0), (1000, 2.0), (2000, 3.0)],
            )],
        );
        let view = ChartViewState {
            zoom: Some((500, 1500)),
            selected_series: None,
        };
        let model = ChartModel::project("t", &f, &[], &view, 80).unwrap();
        assert_eq!(
            model.series[0].points,
            vec![Point {
                timestamp_ms: 1000,
                value: 2.0
            }]
        );
    }

    #[test]
    fn downsampling_reduces_to_target_width_without_synthesizing_gaps() {
        #[allow(clippy::cast_precision_loss)]
        let points: Vec<(i64, f64)> = (0..100i64).map(|i| (i * 1000, i as f64)).collect();
        let f = frame(FrameKind::Timeseries, vec![labeled_series("node", points)]);
        let model = ChartModel::project("t", &f, &[], &ChartViewState::default(), 10).unwrap();
        assert!(model.series[0].points.len() <= 10);
        assert!(!model.series[0].points.is_empty());
    }

    #[test]
    fn stale_selected_series_index_is_ignored_not_an_error() {
        let f = frame(
            FrameKind::Timeseries,
            vec![labeled_series("node", vec![(0, 1.0)])],
        );
        let view = ChartViewState {
            zoom: None,
            selected_series: Some(99),
        };
        let model = ChartModel::project("t", &f, &[], &view, 80).unwrap();
        assert!(!model.series[0].highlighted);
        assert_eq!(model.current_value, Some(1.0));
    }

    #[test]
    fn series_beyond_cap_are_ranked_and_counted_not_dropped_silently() {
        let series: Vec<Series> = (0..12)
            .map(|i| labeled_series(&format!("job{i}"), vec![(0, f64::from(i))]))
            .collect();
        let f = frame(FrameKind::Timeseries, series);
        let model = ChartModel::project("t", &f, &[], &ChartViewState::default(), 80).unwrap();
        assert_eq!(model.series.len(), MAX_DISPLAYED_SERIES);
        assert_eq!(model.truncated_series_count, 12 - MAX_DISPLAYED_SERIES);
        // Highest-value series (job11) must survive the ranking.
        assert!(model.series.iter().any(|s| s.name == "job=job11"));
    }

    #[test]
    fn selected_series_survives_truncation_even_if_low_ranked() {
        let series: Vec<Series> = (0..12)
            .map(|i| labeled_series(&format!("job{i}"), vec![(0, f64::from(i))]))
            .collect();
        let f = frame(FrameKind::Timeseries, series);
        let view = ChartViewState {
            zoom: None,
            selected_series: Some(0), // lowest value, would normally be dropped
        };
        let model = ChartModel::project("t", &f, &[], &view, 80).unwrap();
        assert!(model
            .series
            .iter()
            .any(|s| s.name == "job=job0" && s.highlighted));
    }

    #[test]
    fn severity_picks_most_extreme_fired_band() {
        let bands = vec![
            ThresholdBand {
                name: "warn".into(),
                op: ThresholdOp::Gte,
                value: 0.75,
            },
            ThresholdBand {
                name: "crit".into(),
                op: ThresholdOp::Gte,
                value: 0.90,
            },
        ];
        assert_eq!(Severity::evaluate(0.5, &bands), Severity::Ok);
        assert_eq!(
            Severity::evaluate(0.80, &bands),
            Severity::Breached(bands[0].clone())
        );
        assert_eq!(
            Severity::evaluate(0.95, &bands),
            Severity::Breached(bands[1].clone())
        );
    }

    #[test]
    fn severity_marker_and_label_carry_meaning_without_color() {
        assert_eq!(Severity::Ok.marker(), '●');
        assert_eq!(Severity::Ok.label(), "ok");
        let breached = Severity::Breached(ThresholdBand {
            name: "crit".into(),
            op: ThresholdOp::Gte,
            value: 0.9,
        });
        assert_eq!(breached.marker(), '▲');
        assert_eq!(breached.label(), "crit");
    }

    #[test]
    fn render_text_is_deterministic_across_calls() {
        let f = frame(
            FrameKind::Timeseries,
            vec![labeled_series(
                "node",
                vec![(0, 1.0), (1000, 2.0), (2000, 0.5)],
            )],
        );
        let thresholds = vec![ValidatedThreshold {
            name: "warn".into(),
            op: ThresholdOp::Gte,
            value: 1.5,
        }];
        let model =
            ChartModel::project("CPU", &f, &thresholds, &ChartViewState::default(), 80).unwrap();
        let first = model.render_text();
        let second = model.render_text();
        assert_eq!(first, second);
        assert!(first.contains("CPU"));
        assert!(first.contains("thresholds:"));
        assert!(first.contains("warn"));
    }

    #[test]
    fn instant_vector_is_a_supported_kind() {
        let f = frame(
            FrameKind::InstantVector,
            vec![labeled_series("node", vec![(0, 1.0)])],
        );
        assert!(ChartModel::project("t", &f, &[], &ChartViewState::default(), 80).is_ok());
    }
}
