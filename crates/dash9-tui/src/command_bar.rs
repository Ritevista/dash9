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
/// one is pending) without this module knowing why.
pub fn draw_command_bar(
    frame: &mut Frame,
    area: Rect,
    log: &[LogLine],
    input: Option<&str>,
    hint: &str,
) {
    let [log_area, input_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(INPUT_HEIGHT)]).areas(area);

    draw_log(frame, log_area, log);
    draw_input(frame, input_area, input, hint);
}

fn draw_log(frame: &mut Frame, area: Rect, log: &[LogLine]) {
    let block = Block::default().borders(Borders::ALL).title("log");
    if log.is_empty() {
        frame.render_widget(Paragraph::new("(empty)").block(block), area);
        return;
    }

    // Only the most recent lines that fit are shown — v1 has no
    // scrollback, same "no scrolling" limitation as the panel grid.
    let visible_rows = usize::from(area.height.saturating_sub(2));
    let start = log.len().saturating_sub(visible_rows);
    let text = log[start..]
        .iter()
        .map(format_log_line)
        .collect::<Vec<_>>()
        .join("\n");

    frame.render_widget(Paragraph::new(text).block(block), area);
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
            .draw(|f| draw_command_bar(f, f.area(), &[], None, "press : to enter a command"))
            .unwrap();
    }

    #[test]
    fn log_longer_than_visible_area_draws_without_panicking() {
        let log: Vec<LogLine> = (0..50)
            .map(|i| LogLine::Result(format!("line {i}")))
            .collect();
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| draw_command_bar(f, f.area(), &log, None, "press : to enter a command"))
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
                );
            })
            .unwrap();
    }

    #[test]
    fn zero_area_draws_without_panicking() {
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| {
                draw_command_bar(f, Rect::new(0, 0, 0, 0), &[], None, "hint");
            })
            .unwrap();
    }
}
