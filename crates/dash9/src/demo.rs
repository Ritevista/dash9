//! `dash9 demo`: a self-contained event loop over synthetic data.
//!
//! This is the only place dash9 fabricates a `Frame` instead of
//! getting one from a datasource adapter — it exists to exercise the
//! `ChartModel` projection and Ratatui draw pipeline end to end
//! without a live Prometheus instance to point at.
//!
//! `--assist` (see `docs/specs/assist.md` Section K) layers a scripted
//! assistant walkthrough on top of the same chart: every few seconds
//! it "asks" the next canned demo fixture through a real
//! `AssistSession<FixtureLlmClient>` — the real contract/validate
//! path, zero network — and shows the result in a log panel. There is
//! no live Prometheus datasource behind this demo's `q`/`range`
//! commands, so "auto-run" here means "logged immediately, no
//! confirmation gate," not "produced a new Frame from a real query" —
//! the chart keeps animating from its own synthetic signal regardless
//! of what the assistant proposes. Pretending otherwise would violate
//! the "never fabricate a result" rule (Section J).

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

pub fn run(assist: bool) -> anyhow::Result<()> {
    if assist {
        return run_assist_walkthrough();
    }
    run_plain()
}

fn run_plain() -> anyhow::Result<()> {
    let started_at = Instant::now();
    let thresholds = demo_thresholds();

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
            terminal.draw(|f| draw_chart(f, f.area(), &model, true, false, true))?;

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

#[cfg(not(feature = "assist"))]
fn run_assist_walkthrough() -> anyhow::Result<()> {
    anyhow::bail!(
        "dash9 was built without the `assist` feature; rebuild with `cargo build --features assist`"
    )
}

#[cfg(feature = "assist")]
fn run_assist_walkthrough() -> anyhow::Result<()> {
    use dash9_assist::{
        ActiveDatasourceMetadata, AssistConfig, AssistContext, AssistSession, DatasourceSummary,
        FixtureLlmClient, TimeRangeSummary, DEMO_FIXTURES_JSON,
    };

    /// How often the walkthrough "types" the next canned request.
    const CYCLE: Duration = Duration::from_secs(6);
    const MAX_LOG_LINES: usize = 16;

    let started_at = Instant::now();
    let thresholds = demo_thresholds();

    let client = FixtureLlmClient::from_json(DEMO_FIXTURES_JSON)
        .map_err(|e| anyhow::anyhow!("invalid demo fixtures: {e}"))?;
    let config =
        AssistConfig::from_toml_str("base_url = \"local-fixture\"\nmodel = \"demo-fixture\"\n")
            .map_err(|e| anyhow::anyhow!("invalid demo assist config: {e}"))?;
    let workspace_root = std::env::current_dir().unwrap_or_default();
    let mut session = AssistSession::new(client, &config, workspace_root);

    let requests = [
        "show cpu load over the last hour",
        "save this as examples/load.toml",
        "what's the weather today",
    ];
    let mut next_request_index = 0usize;
    let mut next_request_at = Instant::now() + Duration::from_secs(2);
    let mut log: Vec<String> =
        vec!["press q to quit, a to toggle the assistant on/off".to_string()];

    let context = AssistContext {
        datasources: vec![DatasourceSummary {
            name: "prom".to_string(),
            datasource_type: "prometheus".to_string(),
        }],
        active_datasource_metadata: Some(ActiveDatasourceMetadata {
            datasource_name: "prom".to_string(),
            metric_names: vec!["node_load1".to_string(), "up".to_string()],
            label_keys: vec!["instance".to_string(), "job".to_string()],
        }),
        dashboard_toml: None,
        time_range: TimeRangeSummary {
            start_ms: 0,
            end_ms: 3_600_000,
        },
    };

    let runtime_handle = tokio::runtime::Handle::current();

    ratatui::run(|terminal| -> anyhow::Result<()> {
        loop {
            let width = terminal.size()?.width;
            let frame = synthetic_frame(started_at);
            let model = ChartModel::project(
                "demo --assist: synthetic saturation signal",
                &frame,
                &thresholds,
                &ChartViewState::default(),
                usize::from(width),
            )?;

            terminal.draw(|f| {
                let chunks = ratatui::layout::Layout::default()
                    .direction(ratatui::layout::Direction::Vertical)
                    .constraints([
                        ratatui::layout::Constraint::Percentage(65),
                        ratatui::layout::Constraint::Percentage(35),
                    ])
                    .split(f.area());
                draw_chart(f, chunks[0], &model, true, false, true);
                draw_log_panel(f, chunks[1], &log, &session.status());
            })?;

            if Instant::now() >= next_request_at {
                let request = requests[next_request_index % requests.len()];
                next_request_index += 1;
                next_request_at = Instant::now() + CYCLE;

                log.push(format!("> {request}"));
                // `main` is already `#[tokio::main]`, i.e. we are
                // running on a tokio worker thread — a plain
                // `Handle::block_on` here would panic ("cannot start
                // a runtime from within a runtime"). `block_in_place`
                // is the sanctioned escape hatch on the multi-threaded
                // runtime: it hands this thread's other work to
                // another worker for the duration of the nested
                // `block_on`, which is fine here since `ask()` against
                // `FixtureLlmClient` never actually awaits real I/O.
                let outcome = tokio::task::block_in_place(|| {
                    runtime_handle.block_on(session.ask(&context, request, true))
                });
                append_outcome(&mut log, outcome, epoch_ms_now());
                if log.len() > MAX_LOG_LINES {
                    let excess = log.len() - MAX_LOG_LINES;
                    log.drain(0..excess);
                }
            }

            if event::poll(TICK)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Char('a') => {
                                session.set_enabled(!session.is_enabled());
                                log.push(format!(
                                    "assistant turned {}",
                                    if session.is_enabled() { "on" } else { "off" }
                                ));
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    })
}

/// Turns one `ask()` outcome into log lines. Every command — whether
/// auto-run or a proposal — becomes a `SessionLogEntry` marked
/// `CommandSource::Assistant` before it's rendered (Section I: no
/// invisible assistant action, and the same shape a human's command
/// would use), even though this demo only ever displays it as text
/// rather than keeping a persisted, replayable log — that's `dash9
/// open`'s job once it exists.
#[cfg(feature = "assist")]
fn append_outcome(log: &mut Vec<String>, outcome: dash9_assist::AssistOutcome, timestamp_ms: i64) {
    use dash9_assist::{AssistOutcome, ProposedCommand};
    use dash9_core::{CommandSource, SessionLogEntry};

    match outcome {
        AssistOutcome::Turn(turn) => {
            if let Some(sentence) = turn.intent_sentence {
                log.push(sentence);
            }
            for command in turn.commands {
                let (tag, command) = match command {
                    ProposedCommand::AutoRun(cmd) => ("auto", cmd),
                    ProposedCommand::Proposal(cmd) => ("proposal, press to apply", cmd),
                };
                let entry = SessionLogEntry {
                    source: CommandSource::Assistant,
                    command_text: format!("{command:?}"),
                    timestamp_ms,
                };
                log.push(format!("  [{tag}] {}", entry.command_text));
            }
        }
        AssistOutcome::Refusal(sentence) => log.push(format!("assistant: {sentence}")),
        AssistOutcome::Failed(err) => log.push(format!("assistant error: {err}")),
    }
}

#[cfg(feature = "assist")]
fn draw_log_panel(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    log: &[String],
    status: &dash9_assist::AssistStatusModel,
) {
    use ratatui::widgets::{Block, Borders, Paragraph};

    let mut text = log.join("\n");
    text.push('\n');
    text.push_str(&status.render_text());

    let paragraph =
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("assist"));
    frame.render_widget(paragraph, area);
}

fn demo_thresholds() -> Vec<ValidatedThreshold> {
    vec![
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
    ]
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
