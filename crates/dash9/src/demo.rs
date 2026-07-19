//! `dash9 demo`: a self-contained event loop over synthetic data.
//!
//! This is the only place dash9 fabricates a `Frame` instead of
//! getting one from a datasource adapter — it exists to exercise the
//! `ChartModel` projection and Ratatui draw pipeline end to end
//! without a live Prometheus instance to point at.

use std::collections::BTreeMap;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use dash9_core::{Frame, FrameKind, FrameMeta, Point, Series, ThresholdOp, ValidatedThreshold};
use dash9_tui::chart::{ChartModel, ChartViewState};
use dash9_tui::draw_chart;

/// How many samples the synthetic series carries; deliberately more
/// than most terminal widths so the demo also exercises downsampling.
const HISTORY_POINTS: i64 = 120;
const TICK: Duration = Duration::from_millis(250);

pub fn run() -> anyhow::Result<()> {
    let started_at = Instant::now();
    let thresholds = vec![
        ValidatedThreshold {
            name: "warn".to_string(),
            op: ThresholdOp::Gte,
            value: 0.75,
        },
        ValidatedThreshold {
            name: "crit".to_string(),
            op: ThresholdOp::Gte,
            value: 0.90,
        },
    ];

    ratatui::run(|terminal| -> anyhow::Result<()> {
        loop {
            let width = terminal.size()?.width;
            let frame = synthetic_frame(started_at);
            let model = ChartModel::project(
                "demo: synthetic saturation signal — press q to quit",
                &frame,
                &thresholds,
                &ChartViewState::default(),
                usize::from(width),
            )?;
            terminal.draw(|f| draw_chart(f, f.area(), &model))?;

            if event::poll(TICK)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press
                        && matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
                    {
                        return Ok(());
                    }
                }
            }
        }
    })
}

#[allow(clippy::cast_precision_loss)]
fn synthetic_frame(started_at: Instant) -> Frame {
    let elapsed_s = started_at.elapsed().as_secs_f64();
    let now_epoch_ms = epoch_ms_now();
    let tick_ms = i64::try_from(TICK.as_millis()).unwrap_or(250);

    let mut labels = BTreeMap::new();
    labels.insert("instance".to_string(), "localhost:9100".to_string());
    labels.insert("job".to_string(), "demo".to_string());

    let points = (0..HISTORY_POINTS)
        .map(|i| {
            let offset_ms = (HISTORY_POINTS - 1 - i) * tick_ms;
            let t = elapsed_s - offset_ms as f64 / 1000.0;
            let value = (0.55 + 0.30 * (t / 4.0).sin() + 0.10 * (t / 0.6).sin()).clamp(0.0, 1.0);
            Point {
                timestamp_ms: now_epoch_ms - offset_ms,
                value,
            }
        })
        .collect();

    Frame {
        kind: FrameKind::Timeseries,
        series: vec![Series { labels, points }],
        table: None,
        meta: FrameMeta {
            query: "demo synthetic signal".to_string(),
            datasource: "demo".to_string(),
            executed_at_ms: now_epoch_ms,
            warnings: vec![],
        },
    }
}

fn epoch_ms_now() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_frame_is_a_well_formed_timeseries() {
        let frame = synthetic_frame(Instant::now());
        assert_eq!(frame.kind, FrameKind::Timeseries);
        assert_eq!(frame.series.len(), 1);
        assert_eq!(
            i64::try_from(frame.series[0].points.len()).unwrap(),
            HISTORY_POINTS
        );
        assert!(frame.series[0]
            .points
            .iter()
            .all(|p| (0.0..=1.0).contains(&p.value)));
        // Points are in ascending timestamp order and end at "now".
        let points = &frame.series[0].points;
        assert!(points
            .windows(2)
            .all(|w| w[0].timestamp_ms < w[1].timestamp_ms));
    }

    #[test]
    fn synthetic_frame_projects_onto_a_chart_model() {
        let frame = synthetic_frame(Instant::now());
        let model =
            ChartModel::project("demo", &frame, &[], &ChartViewState::default(), 80).unwrap();
        assert_eq!(model.series.len(), 1);
        assert!(model.current_value.is_some());
    }
}
