//! The output pane: the full text of the most recent `LogLine::Result`
//! (`/help`, query results, export confirmations, error messages, …),
//! rendered in its own dedicated area instead of being crammed into the
//! compact command log (`command_bar.rs`) alongside command echoes —
//! mixing full result text into that thin scrolling strip made both hard
//! to read. Pure rendering, no I/O — same rule as every other module
//! here; `ShellState.log` (`dash9_core::LogLine`) stays the single
//! source of truth, this is a second, filtered view onto it.

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use dash9_core::LogLine;

use crate::theme;

/// Lower/upper bound on the pane's height (border rows included) — small
/// enough that a one-line result (`"range set to 5m"`) doesn't waste
/// space, capped so a very long result (a big `/help` listing) doesn't
/// crowd out the panel grid above it. `output_height` computes the
/// actual value for a given frame; these are its clamp bounds.
pub const MIN_OUTPUT_HEIGHT: u16 = 3;
pub const MAX_OUTPUT_HEIGHT: u16 = 12;

/// The most recent `Result` in `log`, ignoring `Command` entries —
/// `None` before anything has run yet.
pub fn latest_result_text(log: &[LogLine]) -> Option<&str> {
    log.iter().rev().find_map(|line| match line {
        LogLine::Result(text) => Some(text.as_str()),
        LogLine::Command(_) => None,
    })
}

/// How tall the output pane should be this frame: big enough for the
/// latest result's line count (plus borders), clamped to
/// `[MIN_OUTPUT_HEIGHT, MAX_OUTPUT_HEIGHT]` and never more than
/// `available` — "kept dynamic" rather than a fixed height that's either
/// wasted space for a one-liner or too cramped for a long listing.
pub fn output_height(log: &[LogLine], available: u16) -> u16 {
    let content_lines = latest_result_text(log).map_or(1, |text| text.lines().count().max(1));
    let needed = u16::try_from(content_lines.saturating_add(2)).unwrap_or(u16::MAX);
    needed
        .clamp(MIN_OUTPUT_HEIGHT, MAX_OUTPUT_HEIGHT)
        .min(available)
}

/// Draws the latest result's full text into `area` — a placeholder
/// before anything has run yet, never a blank pane (same convention
/// `detail_view.rs`'s data placeholder already uses).
pub fn draw_output(frame: &mut Frame, area: Rect, log: &[LogLine]) {
    let block = Block::default().borders(Borders::ALL).title("output");
    match latest_result_text(log) {
        Some(text) => {
            frame.render_widget(
                Paragraph::new(text.to_string())
                    .style(Style::default().fg(theme::TEXT))
                    .block(block),
                area,
            );
        }
        None => {
            frame.render_widget(
                Paragraph::new("(no output yet)")
                    .style(Style::default().fg(theme::MUTED))
                    .block(block),
                area,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash9_core::{CommandSource, SessionLogEntry};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn backend(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).unwrap()
    }

    fn command_line(text: &str) -> LogLine {
        LogLine::Command(SessionLogEntry {
            source: CommandSource::User,
            command_text: text.to_string(),
            timestamp_ms: 0,
        })
    }

    #[test]
    fn latest_result_text_ignores_commands_and_picks_the_newest_result() {
        let log = vec![
            LogLine::Result("first".to_string()),
            command_line("range 5m"),
            LogLine::Result("second".to_string()),
        ];
        assert_eq!(latest_result_text(&log), Some("second"));
    }

    #[test]
    fn latest_result_text_is_none_before_any_result() {
        let log = vec![command_line("range 5m")];
        assert_eq!(latest_result_text(&log), None);
        assert_eq!(latest_result_text(&[]), None);
    }

    #[test]
    fn output_height_grows_with_content_up_to_the_max() {
        let one_liner = vec![LogLine::Result("range set to 5m".to_string())];
        assert_eq!(output_height(&one_liner, 20), MIN_OUTPUT_HEIGHT);

        let big = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let long = vec![LogLine::Result(big)];
        assert_eq!(output_height(&long, 20), MAX_OUTPUT_HEIGHT);
    }

    #[test]
    fn output_height_never_exceeds_available_space() {
        let big = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let long = vec![LogLine::Result(big)];
        assert_eq!(output_height(&long, 5), 5);
    }

    #[test]
    fn output_height_before_any_result_is_the_minimum() {
        assert_eq!(output_height(&[], 20), MIN_OUTPUT_HEIGHT);
    }

    #[test]
    fn draws_the_placeholder_before_any_result_without_panicking() {
        let mut terminal = backend(40, 5);
        terminal.draw(|f| draw_output(f, f.area(), &[])).unwrap();
    }

    #[test]
    fn draws_the_latest_result_without_panicking() {
        let log = vec![
            command_line("help"),
            LogLine::Result("Commands — prefix with /\n  ds  manage datasources".to_string()),
        ];
        let mut terminal = backend(40, 8);
        terminal.draw(|f| draw_output(f, f.area(), &log)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(content.contains("Commands"));
    }

    #[test]
    fn zero_area_draws_without_panicking() {
        let mut terminal = backend(40, 5);
        terminal
            .draw(|f| draw_output(f, Rect::new(0, 0, 0, 0), &[]))
            .unwrap();
    }
}
