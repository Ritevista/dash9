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
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use dash9_core::LogLine;

use crate::pane::pane_block;
use crate::theme;

/// Bottom-left border hint shown only when the output pane is focused and
/// not itself being shadowed by the command box editing (`show_hint`,
/// same `focused && !editing` gating every other pane's hint uses —
/// `docs/specs/open.md` Section G.2).
const OUTPUT_HINT: &str = "PageUp/PageDown scroll";

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

/// How far `scroll` (`ShellState::output_scroll`) can grow before it stops
/// revealing anything new, given `visible_rows` content rows actually
/// available (the pane's rendered height minus its two border rows) —
/// self-clamping the same way `layout::max_grid_scroll` is for the panel
/// grid; the composition root uses this to clamp `output_scroll` before
/// passing it to `draw_output`, since `ShellState` itself has no notion of
/// terminal size.
pub fn max_output_scroll(log: &[LogLine], visible_rows: u16) -> usize {
    let content_lines = latest_result_text(log).map_or(1, |text| text.lines().count().max(1));
    content_lines.saturating_sub(usize::from(visible_rows))
}

/// Draws the latest result's full text into `area` — a placeholder
/// before anything has run yet, never a blank pane (same convention
/// `detail_view.rs`'s data placeholder already uses). `scroll` (already
/// clamped by the caller against `max_output_scroll`) is lines down from
/// the top, opposite anchor from the log's tail-anchored scroll — see
/// `ShellState::output_scroll`'s docs for why. `focused`/`show_hint`
/// follow the same shared-chrome convention every other pane uses
/// (`pane::pane_block`, `docs/specs/open.md` Section G.2): `focused`
/// drives the border/name color, `show_hint` (`focused && !editing`,
/// computed by the caller) gates whether the scroll hint is actually
/// shown — while the command box is editing, `PageUp`/`PageDown` scroll
/// the log instead, so a focused-but-editing output pane must not claim a
/// hint that wouldn't currently do what it says.
pub fn draw_output(
    frame: &mut Frame,
    area: Rect,
    log: &[LogLine],
    scroll: usize,
    focused: bool,
    show_hint: bool,
) {
    let block = pane_block("output", focused, None, show_hint.then_some(OUTPUT_HINT));
    match latest_result_text(log) {
        Some(text) => {
            frame.render_widget(
                Paragraph::new(text.to_string())
                    .style(Style::default().fg(theme::TEXT))
                    .scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0))
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
        terminal
            .draw(|f| draw_output(f, f.area(), &[], 0, false, false))
            .unwrap();
    }

    #[test]
    fn draws_the_latest_result_without_panicking() {
        let log = vec![
            command_line("help"),
            LogLine::Result("Commands — prefix with /\n  ds  manage datasources".to_string()),
        ];
        let mut terminal = backend(40, 8);
        terminal
            .draw(|f| draw_output(f, f.area(), &log, 0, false, false))
            .unwrap();
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
            .draw(|f| draw_output(f, Rect::new(0, 0, 0, 0), &[], 0, false, false))
            .unwrap();
    }

    #[test]
    fn focused_pane_shows_the_scroll_hint_only_when_show_hint_is_true() {
        let log = vec![LogLine::Result("hello".to_string())];
        let mut terminal = backend(40, 5);
        terminal
            .draw(|f| draw_output(f, f.area(), &log, 0, true, true))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(content.contains("scroll"), "{content}");
    }

    #[test]
    fn focused_but_editing_shows_no_hint() {
        let log = vec![LogLine::Result("hello".to_string())];
        let mut terminal = backend(40, 5);
        terminal
            .draw(|f| draw_output(f, f.area(), &log, 0, true, false))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            !content.contains("scroll"),
            "focused-but-editing must not claim the scroll hint: {content}"
        );
    }

    #[test]
    fn scrolling_reveals_later_lines_and_hides_earlier_ones() {
        let big = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let log = vec![LogLine::Result(big)];
        let mut terminal = backend(40, 8);
        terminal
            .draw(|f| draw_output(f, f.area(), &log, 10, false, false))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(content.contains("line 10"), "{content}");
        assert!(!content.contains("line 0\n"), "{content}");
    }

    #[test]
    fn max_output_scroll_is_zero_when_everything_already_fits() {
        let log = vec![LogLine::Result("one\ntwo".to_string())];
        assert_eq!(max_output_scroll(&log, 10), 0);
    }

    #[test]
    fn max_output_scroll_grows_with_content_past_the_visible_rows() {
        let big = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let log = vec![LogLine::Result(big)];
        assert_eq!(max_output_scroll(&log, 10), 20);
    }
}
