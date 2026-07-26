//! Command bar / session log rendering: pure Ratatui draw code, no
//! I/O (same rule as every other module in this crate). State — the
//! growing log, whether the bar is in edit mode, the text buffer — is
//! owned by the `dash9` binary's composition root; this module only
//! ever turns what it's handed into draw calls.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use dash9_core::{CommandSource, LogLine, SessionLogEntry};

use crate::pane::pane_block;
use crate::theme;

/// Rows reserved for the input line's own bordered block (1 content
/// row + top/bottom border).
const INPUT_HEIGHT: u16 = 3;

/// Bottom-left border hint shown only when the log has `Tab`-focus and
/// isn't itself shadowed by command-box editing — same `focused &&
/// !editing` gating every other pane's hint uses (`docs/specs/open.md`
/// Section G.2), and the same hint text `output::draw_output` shows for
/// the same reason: both panes share the same scroll mechanism.
const LOG_HINT: &str = "PageUp/PageDown scroll";

/// Lower/upper bound on the log's default (unmaximized) height, border
/// rows included — same values `crate::output`'s `MIN_OUTPUT_HEIGHT`/
/// `MAX_OUTPUT_HEIGHT` use, for the same reason: small enough that an
/// empty or one-line log doesn't waste space, capped so it doesn't
/// consume every leftover row a short dashboard or tall terminal leaves
/// lying around (`docs/specs/open.md` Section F — the log used to get
/// whatever was left via an uncapped `Constraint::Min(0)`, which meant
/// the *least*-used pane silently claimed the *most* screen space on a
/// short dashboard; confirmed live, 17 blank rows on a 50-row terminal).
/// `Region::Log` maximize (`+`, Section F) is the escape hatch for
/// actually wanting more than this.
pub const MIN_LOG_HEIGHT: u16 = 3;
pub const MAX_LOG_HEIGHT: u16 = 12;

/// The log's own `Region::Log` chrome (`docs/specs/open.md` Section E) —
/// bundled into one struct, not two more loose `bool` params on
/// `draw_command_bar`, purely to stay under `clippy::too_many_arguments`;
/// same shared-pane convention (`focused`/`show_hint`) every other
/// bordered area uses.
#[derive(Debug, Clone, Copy)]
pub struct LogFocus {
    pub focused: bool,
    pub show_hint: bool,
}

/// Draws the scrollable command log (above) and the command-bar input
/// line (below) into `area`. The log here is command *echoes* only
/// (`"> /help"`, `"> range 5m"`) — a compact audit trail of what was
/// typed and when; `LogLine::Result` entries (help text, query results,
/// error messages) are deliberately excluded and rendered elsewhere
/// instead (`crate::output::draw_output`), since mixing full result text
/// into this compact strip made both hard to read. `input` is
/// `Some(buffer)` while the user is typing a command (`:` keybinding),
/// `None` otherwise — in which case `hint` is shown instead, letting the
/// composition root surface state-dependent guidance (e.g. "y/n confirm
/// proposal" only while one is pending) without this module knowing why.
/// `scroll` counts lines up from the newest (0 = pinned to the tail) —
/// the composition root owns the value (`ShellState::log_scroll`,
/// PageUp/PageDown), this module just renders it.
pub fn draw_command_bar(
    frame: &mut Frame,
    area: Rect,
    log: &[LogLine],
    input: Option<&str>,
    hint: &str,
    scroll: usize,
    log_focus: LogFocus,
) {
    let [log_area, input_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(INPUT_HEIGHT)]).areas(area);

    draw_log(frame, log_area, log, scroll, log_focus);
    draw_input(frame, input_area, input, hint);
}

/// `Command` entries only, each rendered as one `"> ..."`/`"* ..."` line
/// (`format_command_line`) then split on embedded newlines — shared by
/// `draw_log` and `log_height` so the two can never disagree about what
/// counts as a line.
fn command_lines(log: &[LogLine]) -> Vec<String> {
    log.iter()
        .filter_map(|line| match line {
            LogLine::Command(entry) => Some(format_command_line(entry)),
            LogLine::Result(_) => None,
        })
        .flat_map(|line| {
            line.split('\n')
                .map(str::to_string)
                .collect::<Vec<_>>()
                .into_iter()
        })
        .collect()
}

/// How tall the log's default (unmaximized) area should be this frame —
/// big enough for its content, clamped to `[MIN_LOG_HEIGHT,
/// MAX_LOG_HEIGHT]` and never more than `available`. Mirrors
/// `crate::output::output_height` exactly, including the reasoning: a
/// short/empty log shouldn't waste space, and an uncapped one
/// shouldn't either, in the other direction — see `MAX_LOG_HEIGHT`'s
/// docs for the concrete bug this fixes. Content rows only; add
/// `INPUT_HEIGHT` (or call `command_bar_height`) for the full bar.
pub fn log_height(log: &[LogLine], available: u16) -> u16 {
    let content_lines = command_lines(log).len().max(1);
    let needed = u16::try_from(content_lines.saturating_add(2)).unwrap_or(u16::MAX);
    needed.clamp(MIN_LOG_HEIGHT, MAX_LOG_HEIGHT).min(available)
}

/// `log_height` plus the input line's own fixed height — the composition
/// root's single up-front reservation for the whole command bar
/// (mirrors `output_height`'s role for the output pane), used instead of
/// the old unbounded `Constraint::Min(0)` that let the bar (and so the
/// log) silently absorb every leftover row (`MAX_LOG_HEIGHT`'s docs).
pub fn command_bar_height(log: &[LogLine], available: u16) -> u16 {
    log_height(log, available.saturating_sub(INPUT_HEIGHT)) + INPUT_HEIGHT
}

fn draw_log(frame: &mut Frame, area: Rect, log: &[LogLine], scroll: usize, log_focus: LogFocus) {
    let title = if scroll > 0 {
        "log (scrolled — PageDown to catch up)"
    } else {
        "log"
    };
    let block = pane_block(
        title,
        log_focus.focused,
        None,
        log_focus.show_hint.then_some(LOG_HINT),
    );

    // Command text is realistically always one line, but the window is
    // still computed over actual rendered lines rather than entry count
    // — same "don't undercount if something someday embeds a newline"
    // discipline `crate::output::draw_output`'s sibling doesn't need
    // (it shows one Result's full text, not a scrolling history).
    let rendered = command_lines(log);
    let lines: Vec<&str> = rendered.iter().map(String::as_str).collect();
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

fn format_command_line(entry: &SessionLogEntry) -> String {
    let marker = match entry.source {
        CommandSource::User => ">",
        CommandSource::Assistant => "*",
    };
    format!("{marker} {}", entry.command_text)
}

fn draw_input(frame: &mut Frame, area: Rect, input: Option<&str>, hint: &str) {
    let focused = input.is_some();
    let (text, style) = match input {
        Some(buffer) => (format!(": {buffer}"), Style::default().fg(theme::FOCUS)),
        None => (hint.to_string(), Style::default().fg(theme::MUTED)),
    };
    // "command" gets the same shared pane chrome as every other region
    // (`docs/specs/open.md` Section E) — no separate hint text here, the
    // input line's own contents already say what's happening (the `:
    // <buffer>` prompt while editing, the passed-in `hint` otherwise).
    let block = pane_block("command", focused, None, None);
    frame.render_widget(Paragraph::new(text).style(style).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn backend(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).unwrap()
    }

    const UNFOCUSED: LogFocus = LogFocus {
        focused: false,
        show_hint: false,
    };

    #[test]
    fn empty_log_draws_without_panicking() {
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| {
                draw_command_bar(
                    f,
                    f.area(),
                    &[],
                    None,
                    "press : to enter a command",
                    0,
                    UNFOCUSED,
                );
            })
            .unwrap();
    }

    fn command_line(text: &str) -> LogLine {
        LogLine::Command(SessionLogEntry {
            source: CommandSource::User,
            command_text: text.to_string(),
            timestamp_ms: 0,
        })
    }

    #[test]
    fn log_longer_than_visible_area_draws_without_panicking() {
        let log: Vec<LogLine> = (0..50).map(|i| command_line(&format!("cmd {i}"))).collect();
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| {
                draw_command_bar(
                    f,
                    f.area(),
                    &log,
                    None,
                    "press : to enter a command",
                    0,
                    UNFOCUSED,
                );
            })
            .unwrap();
    }

    #[test]
    fn scrolled_log_draws_without_panicking() {
        let log: Vec<LogLine> = (0..50).map(|i| command_line(&format!("cmd {i}"))).collect();
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| {
                draw_command_bar(
                    f,
                    f.area(),
                    &log,
                    None,
                    "press : to enter a command",
                    20,
                    UNFOCUSED,
                );
            })
            .unwrap();
    }

    #[test]
    fn only_command_echoes_are_shown_result_lines_are_excluded() {
        let log = vec![
            command_line("range 5m"),
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
                    UNFOCUSED,
                );
            })
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(content.contains("range 5m"), "user command echo shown");
        assert!(
            content.contains("panel type gauge"),
            "assistant command echo shown"
        );
        assert!(
            !content.contains("range set to 5m"),
            "the Result's text must not appear in the command log"
        );
    }

    #[test]
    fn zero_area_draws_without_panicking() {
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| {
                draw_command_bar(f, Rect::new(0, 0, 0, 0), &[], None, "hint", 0, UNFOCUSED);
            })
            .unwrap();
    }

    #[test]
    fn many_command_lines_are_sliced_by_rendered_row_newest_visible_at_scroll_zero() {
        // 20 separate `Command` entries (the realistic shape a long
        // session's command log actually takes) must still be windowed
        // by rendered row so the newest is visible and the oldest has
        // scrolled off — the same `visible_window` mechanism, exercised
        // end to end through `draw_log`'s filtering rather than a single
        // pathological multi-line entry (which can no longer reach this
        // box now that `LogLine::Result` is filtered out).
        let log: Vec<LogLine> = (0..20)
            .map(|i| command_line(&format!("inner {i}")))
            .collect();
        let mut terminal = backend(60, 8);
        terminal
            .draw(|f| draw_command_bar(f, f.area(), &log, None, "hint", 0, UNFOCUSED))
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

    #[test]
    fn log_height_before_any_command_is_the_minimum() {
        assert_eq!(log_height(&[], 20), MIN_LOG_HEIGHT);
    }

    /// Regression test for the real bug: an unbounded log used to
    /// silently absorb every leftover terminal row (`MAX_LOG_HEIGHT`'s
    /// docs — 17 blank rows on a 50-row terminal, confirmed live). It
    /// must stay capped regardless of how much `available` space there
    /// is.
    #[test]
    fn log_height_never_exceeds_the_max_even_with_lots_of_available_space() {
        let log: Vec<LogLine> = (0..50).map(|i| command_line(&format!("cmd {i}"))).collect();
        assert_eq!(log_height(&log, 200), MAX_LOG_HEIGHT);
    }

    #[test]
    fn log_height_grows_with_content_up_to_the_max() {
        let one_liner = vec![command_line("range 5m")];
        assert_eq!(log_height(&one_liner, 20), MIN_LOG_HEIGHT);
    }

    #[test]
    fn log_height_never_exceeds_available_space() {
        let log: Vec<LogLine> = (0..50).map(|i| command_line(&format!("cmd {i}"))).collect();
        assert_eq!(log_height(&log, 5), 5);
    }

    #[test]
    fn command_bar_height_adds_the_input_line_to_log_height() {
        assert_eq!(
            command_bar_height(&[], 20),
            MIN_LOG_HEIGHT + INPUT_HEIGHT,
            "empty log: minimum content height plus the input line"
        );
    }

    #[test]
    fn focused_log_shows_the_scroll_hint_only_when_show_hint_is_true() {
        let log = vec![command_line("range 5m")];
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| {
                draw_command_bar(
                    f,
                    f.area(),
                    &log,
                    None,
                    "hint",
                    0,
                    LogFocus {
                        focused: true,
                        show_hint: true,
                    },
                );
            })
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
    fn focused_but_editing_log_shows_no_scroll_hint() {
        let log = vec![command_line("range 5m")];
        let mut terminal = backend(60, 10);
        terminal
            .draw(|f| {
                draw_command_bar(
                    f,
                    f.area(),
                    &log,
                    None,
                    "hint",
                    0,
                    LogFocus {
                        focused: true,
                        show_hint: false,
                    },
                );
            })
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
}
