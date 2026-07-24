//! Command bar / session log rendering: pure Ratatui draw code, no
//! I/O (same rule as every other module in this crate). State — the
//! growing log, whether the bar is in edit mode, the text buffer — is
//! owned by the `dash9` binary's composition root; this module only
//! ever turns what it's handed into draw calls.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use dash9_core::{CommandSource, LogLine};

use crate::theme;

/// Rows reserved for the input line's own bordered block (1 content
/// row + top/bottom border).
const INPUT_HEIGHT: u16 = 3;

/// Draws the scrollable log (above) and the command-bar input line
/// (below) into `area`. `input` is `Some(buffer)` while the user is
/// typing a command (`:` keybinding), `None` otherwise — in which
/// case `hint` is shown instead, letting the composition root surface
/// state-dependent guidance (e.g. "y/n confirm proposal" only while
/// one is pending) without this module knowing why. `scroll` counts
/// lines up from the newest (0 = pinned to the tail) — the
/// composition root owns the value (`ShellState::log_scroll`,
/// PageUp/PageDown), this module just renders it.
pub fn draw_command_bar(
    frame: &mut Frame,
    area: Rect,
    log: &[LogLine],
    input: Option<&str>,
    hint: &str,
    scroll: usize,
) {
    let [log_area, input_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(INPUT_HEIGHT)]).areas(area);

    draw_log(frame, log_area, log, scroll);
    draw_input(frame, input_area, input, hint);
}

fn draw_log(frame: &mut Frame, area: Rect, log: &[LogLine], scroll: usize) {
    let title = if scroll > 0 {
        "log (scrolled — PageDown to catch up)"
    } else {
        "log"
    };
    let block = Block::default().borders(Borders::ALL).title(title);

    // A single `LogLine` (e.g. a multi-paragraph `/help` result) can
    // render as many terminal rows, so the visible window is computed
    // over actual rendered lines, not `LogLine` entries — slicing by
    // entry count under-counts whenever an entry embeds newlines,
    // which used to silently clip the newest output off the bottom
    // of the area instead of showing it.
    let rendered: Vec<String> = log.iter().map(format_log_line).collect();
    let lines: Vec<&str> = rendered.iter().flat_map(|line| line.split('\n')).collect();
    if lines.is_empty() {
        frame.render_widget(Paragraph::new("(empty)").block(block), area);
        return;
    }

    let visible_rows = usize::from(area.height.saturating_sub(2));
    let window = visible_window(lines.len(), visible_rows, scroll);
    let text = lines[window].join("\n");

    frame.render_widget(Paragraph::new(text).block(block), area);
}

/// Which flattened log lines are visible at a given scroll offset.
/// Self-clamping: scrolling past the top or bottom is a no-op rather
/// than an out-of-bounds slice, so the caller never needs to know how
/// far is "too far."
fn visible_window(
    total_lines: usize,
    visible_rows: usize,
    scroll: usize,
) -> std::ops::Range<usize> {
    let max_scroll = total_lines.saturating_sub(visible_rows);
    let effective_scroll = scroll.min(max_scroll);
    let end = total_lines.saturating_sub(effective_scroll);
    let start = end.saturating_sub(visible_rows);
    start..end
}

fn format_log_line(line: &LogLine) -> String {
    match line {
        LogLine::Command(entry) => {
            let marker = match entry.source {
                CommandSource::User => ">",
                CommandSource::Assistant => "*",
            };
            format!("{marker} {}", entry.command_text)
        }
        LogLine::Result(text) => format!("  {text}"),
    }
}

fn draw_input(frame: &mut Frame, area: Rect, input: Option<&str>, hint: &str) {
    let (text, style) = match input {
        Some(buffer) => (format!(": {buffer}"), Style::default().fg(theme::FOCUS)),
        None => (hint.to_string(), Style::default().fg(theme::MUTED)),
    };
    let block = Block::default().borders(Borders::ALL);
    frame.render_widget(Paragraph::new(text).style(style).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use dash9_core::SessionLogEntry;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn backend(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).unwrap()
    }

    #[test]
    fn empty_log_draws_without_panicking() {
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| draw_command_bar(f, f.area(), &[], None, "press : to enter a command", 0))
            .unwrap();
    }

    #[test]
    fn log_longer_than_visible_area_draws_without_panicking() {
        let log: Vec<LogLine> = (0..50)
            .map(|i| LogLine::Result(format!("line {i}")))
            .collect();
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| draw_command_bar(f, f.area(), &log, None, "press : to enter a command", 0))
            .unwrap();
    }

    #[test]
    fn scrolled_log_draws_without_panicking() {
        let log: Vec<LogLine> = (0..50)
            .map(|i| LogLine::Result(format!("line {i}")))
            .collect();
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| draw_command_bar(f, f.area(), &log, None, "press : to enter a command", 20))
            .unwrap();
    }

    #[test]
    fn command_and_result_lines_and_active_input_draw_without_panicking() {
        let log = vec![
            LogLine::Command(SessionLogEntry {
                source: CommandSource::User,
                command_text: "range 5m".to_string(),
                timestamp_ms: 0,
            }),
            LogLine::Command(SessionLogEntry {
                source: CommandSource::Assistant,
                command_text: "panel type gauge".to_string(),
                timestamp_ms: 0,
            }),
            LogLine::Result("range set to 5m".to_string()),
        ];
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| {
                draw_command_bar(
                    f,
                    f.area(),
                    &log,
                    Some("range 1"),
                    "press : to enter a command",
                    0,
                );
            })
            .unwrap();
    }

    #[test]
    fn zero_area_draws_without_panicking() {
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| {
                draw_command_bar(f, Rect::new(0, 0, 0, 0), &[], None, "hint", 0);
            })
            .unwrap();
    }

    #[test]
    fn a_multiline_entry_is_sliced_by_rendered_lines_not_by_entry_count() {
        // One `LogLine` whose text embeds 20 newlines must still be
        // sliceable by actual rendered row, not treated as "1 entry"
        // that either shows entirely or not at all.
        let big = (0..20)
            .map(|i| format!("inner {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let log = vec![LogLine::Result(big)];
        let mut terminal = backend(60, 8);
        terminal
            .draw(|f| draw_command_bar(f, f.area(), &log, None, "hint", 0))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(
            content.contains("inner 19"),
            "the newest inner line must be visible when scroll is 0"
        );
        assert!(
            !content.contains("inner 0 "),
            "the oldest inner line must have scrolled off when scroll is 0"
        );
    }

    #[test]
    fn visible_window_shows_the_tail_when_scroll_is_zero() {
        assert_eq!(visible_window(20, 5, 0), 15..20);
    }

    #[test]
    fn visible_window_scrolls_up_by_the_given_amount() {
        assert_eq!(visible_window(20, 5, 4), 11..16);
    }

    #[test]
    fn visible_window_clamps_at_the_top_instead_of_going_out_of_bounds() {
        assert_eq!(visible_window(20, 5, 100), 0..5);
    }

    #[test]
    fn visible_window_when_everything_fits_shows_all_of_it() {
        assert_eq!(visible_window(3, 10, 0), 0..3);
    }
}
