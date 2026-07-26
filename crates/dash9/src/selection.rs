//! Mouse drag-to-select and clipboard copy (`docs/specs/open.md`
//! Section L). Lives in the `dash9` binary crate, not `dash9-tui`,
//! because it does real terminal I/O (raw stdout writes for the OSC 52
//! escape) — the same boundary `dash9-tui::shell`'s module docs already
//! draw around `ShellState` ("no terminal, filesystem, or network I/O").
//!
//! Selection is screen-coordinate state (terminal cell column/row),
//! not session state, so it lives as a local variable in
//! `open::shell_loop`'s render loop, the same way `grid_viewport_height`
//! does, rather than inside `ShellState`.

use std::io::{self, Write};

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::Modifier;
use ratatui::widgets::Widget;

/// One in-progress or just-finished drag selection, in raw terminal
/// cell coordinates (column, row) — the same coordinate space
/// `crossterm::event::MouseEvent::{column,row}` reports.
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub anchor: (u16, u16),
    pub cursor: (u16, u16),
}

impl Selection {
    pub fn new(at: (u16, u16)) -> Self {
        Self {
            anchor: at,
            cursor: at,
        }
    }

    /// `(start, end)` in reading order (top-to-bottom, left-to-right) —
    /// a drag can go in any direction, but extraction/highlighting only
    /// ever need to walk forward.
    fn ordered(self) -> ((u16, u16), (u16, u16)) {
        if (self.anchor.1, self.anchor.0) <= (self.cursor.1, self.cursor.0) {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    /// Whether this selection covers more than a single cell — a plain
    /// click (`Down` immediately followed by `Up` at the same cell)
    /// clears any prior selection rather than "selecting" one character,
    /// matching how terminal-native click-drag selection behaves.
    pub fn is_empty(self) -> bool {
        self.anchor == self.cursor
    }

    /// Applies reverse-video styling to every selected cell, called
    /// right after the frame's normal content is drawn so the highlight
    /// sits on top of it. Style-only — the buffer's cell *content* is
    /// untouched, so this is safe to call every frame while dragging
    /// without disturbing what `extract_text` later reads.
    pub fn highlight(self, buffer: &mut Buffer) {
        for (row, col_start, col_end) in self.row_spans(buffer.area) {
            let rect = Rect {
                x: col_start,
                y: row,
                width: col_end.saturating_sub(col_start) + 1,
                height: 1,
            };
            buffer.set_style(rect.intersection(buffer.area), Modifier::REVERSED);
        }
    }

    /// The selected text, reading-order, trimmed of trailing whitespace
    /// per line (buffer cells pad unused width with spaces — without
    /// trimming, every line but the last row of a multi-row selection
    /// would carry a full screen-width of trailing blanks) and joined
    /// with `\n` for a multi-row selection. `None` for an empty
    /// selection (nothing to copy).
    pub fn extract_text(self, buffer: &Buffer) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let lines: Vec<String> = self
            .row_spans(buffer.area)
            .into_iter()
            .map(|(row, col_start, col_end)| {
                let mut line = String::new();
                for col in col_start..=col_end {
                    if let Some(cell) = buffer.cell(Position { x: col, y: row }) {
                        line.push_str(cell.symbol());
                    }
                }
                line.trim_end().to_string()
            })
            .collect();
        let text = lines.join("\n");
        (!text.is_empty()).then_some(text)
    }

    /// Per-row `(row, col_start, col_end)` spans (inclusive) the
    /// selection covers, clamped to `area` — linear (reading-order) text
    /// selection, not a rectangular block: the first row runs from the
    /// selection's start column to the row's right edge, the last row
    /// from the row's left edge to the selection's end column, and any
    /// rows in between are covered in full. A single-row selection is
    /// just its own `(start_col, end_col)` span.
    fn row_spans(self, area: Rect) -> Vec<(u16, u16, u16)> {
        let (start, end) = self.ordered();
        let left = area.x;
        let right = area.right().saturating_sub(1);
        let top = area.y.max(start.1);
        let bottom = area.bottom().saturating_sub(1).min(end.1);
        if top > bottom {
            return Vec::new();
        }
        (top..=bottom)
            .map(|row| {
                let col_start = if row == start.1 {
                    start.0.max(left)
                } else {
                    left
                };
                let col_end = if row == end.1 {
                    end.0.min(right)
                } else {
                    right
                };
                (row, col_start.min(col_end), col_end)
            })
            .collect()
    }
}

/// Lets a selection be drawn with `Frame::render_widget(&selection,
/// frame.area())` — `Frame::buffer` is crate-private in `ratatui-core`,
/// so a widget impl is the only way application code gets a `&mut
/// Buffer` to paint the highlight into; `area` is ignored since
/// `highlight`/`row_spans` already clip against the buffer's own area.
impl Widget for &Selection {
    fn render(self, _area: Rect, buf: &mut Buffer) {
        self.highlight(buf);
    }
}

/// Copies `text` to the system clipboard via a bare OSC 52 escape
/// (`\x1b]52;c;<base64>\x07`), written straight to stdout — bypasses
/// tmux's/the terminal's own selection entirely, which is the whole
/// point (`docs/specs/open.md` Section L: dash9 draws its own
/// selection highlight and owns the copy, immune to tmux copy-mode
/// losing track of a selection under live-refreshing panel content).
///
/// **Deliberately unwrapped, even inside tmux.** An earlier version of
/// this function wrapped the sequence in tmux's DCS passthrough
/// (`\x1bPtmux;...\x1b\\`) whenever `$TMUX` was set, on the assumption
/// that tmux needed help forwarding it to the real terminal — that
/// assumption was wrong and the wrapping was actively harmful: tmux's
/// DCS passthrough is gated behind `allow-passthrough`, which defaults
/// to **off**, so the wrapped sequence was silently dropped on any
/// default tmux config (confirmed live: `tmux show-options -g
/// allow-passthrough` → `off`, and the wrapped write produced no new
/// paste buffer). tmux natively recognizes and handles *bare* OSC 52
/// on its own — that's what `set-clipboard on` (tmux's default)
/// configures — so no wrapping is needed or wanted; sending the bare
/// sequence unconditionally is what actually works, confirmed via a
/// live smoke test (`tmux show-buffer` picked it up correctly).
pub fn copy_to_clipboard(text: &str) -> io::Result<()> {
    write_osc52(text, &mut io::stdout())
}

/// The actual byte-producing half of [`copy_to_clipboard`], split out
/// so the regression this function's doc comment describes (an earlier
/// version wrapping the sequence in tmux's DCS passthrough, which
/// `allow-passthrough off` — tmux's default — silently swallows) has a
/// real unit test (`tests::osc52_sequence_is_never_tmux_wrapped`)
/// instead of only being guarded by that comment. Takes any `Write` so
/// the test below can assert on an in-memory buffer instead of needing
/// a real stdout/terminal.
fn write_osc52<W: Write>(text: &str, writer: &mut W) -> io::Result<()> {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    write!(writer, "\x1b]52;c;{encoded}\x07")?;
    writer.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Line;

    #[test]
    fn write_osc52_produces_the_exact_bare_escape_sequence() {
        let mut buf = Vec::new();
        write_osc52("hi", &mut buf).unwrap();
        // base64("hi") == "aGk="
        assert_eq!(buf, b"\x1b]52;c;aGk=\x07");
    }

    /// Regression test for the tmux clipboard bug: an earlier version
    /// wrapped the OSC 52 escape in tmux's DCS passthrough
    /// (`\x1bPtmux;...\x1b\\`) whenever `$TMUX` was set. tmux's DCS
    /// passthrough is gated behind `allow-passthrough`, which defaults
    /// to **off** — so on any default tmux config, the wrapped sequence
    /// was silently dropped and the mouse-selection copy (Section L)
    /// never reached the clipboard, even though `copy_to_clipboard`
    /// itself returned `Ok(())`. Confirmed live in tmux 3.6 (default
    /// config): the wrapped write produced no new `tmux show-buffer`
    /// entry; the identical text sent bare did. tmux handles bare OSC 52
    /// natively via `set-clipboard` (on by default) — no wrapping is
    /// needed or wanted, in or out of tmux, so `write_osc52` must never
    /// emit the `\x1bPtmux;` wrapper regardless of environment.
    #[test]
    fn osc52_sequence_is_never_tmux_wrapped() {
        let mut buf = Vec::new();
        write_osc52("selected text", &mut buf).unwrap();
        let written = String::from_utf8(buf).unwrap();
        assert!(
            !written.contains("Ptmux"),
            "OSC 52 must never be wrapped in tmux's DCS passthrough: {written:?}"
        );
        assert!(
            written.starts_with("\x1b]52;c;"),
            "must be a bare OSC 52 escape: {written:?}"
        );
        assert!(written.ends_with('\x07'));
    }

    #[test]
    fn osc52_sequence_base64_encodes_arbitrary_text_including_newlines() {
        use base64::Engine as _;

        let mut buf = Vec::new();
        write_osc52("line one\nline two", &mut buf).unwrap();
        let written = String::from_utf8(buf).unwrap();
        let inner = written
            .strip_prefix("\x1b]52;c;")
            .and_then(|s| s.strip_suffix('\x07'))
            .expect("well-formed OSC 52 sequence");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(inner)
            .unwrap();
        assert_eq!(decoded, b"line one\nline two");
    }

    fn buffer_from(lines: &[&str]) -> Buffer {
        let width =
            u16::try_from(lines.iter().map(|l| l.len()).max().unwrap_or(0)).unwrap_or(u16::MAX);
        let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        for (y, line) in lines.iter().enumerate() {
            let row = u16::try_from(y).unwrap_or(u16::MAX);
            Line::from(*line).render(Rect::new(0, row, width, 1), &mut buffer);
        }
        buffer
    }

    #[test]
    fn single_row_selection_extracts_the_substring() {
        let buffer = buffer_from(&["hello world"]);
        let selection = Selection {
            anchor: (0, 0),
            cursor: (4, 0),
        };
        assert_eq!(selection.extract_text(&buffer).as_deref(), Some("hello"));
    }

    #[test]
    fn selection_direction_does_not_matter() {
        let buffer = buffer_from(&["hello world"]);
        let forward = Selection {
            anchor: (0, 0),
            cursor: (4, 0),
        };
        let backward = Selection {
            anchor: (4, 0),
            cursor: (0, 0),
        };
        assert_eq!(
            forward.extract_text(&buffer),
            backward.extract_text(&buffer)
        );
    }

    #[test]
    fn multi_row_selection_joins_with_newlines_and_trims_trailing_padding() {
        let buffer = buffer_from(&["abcdef", "ghijkl", "mnopqr"]);
        let selection = Selection {
            anchor: (3, 0),
            cursor: (2, 2),
        };
        assert_eq!(
            selection.extract_text(&buffer).as_deref(),
            Some("def\nghijkl\nmno")
        );
    }

    #[test]
    fn empty_selection_extracts_nothing() {
        let buffer = buffer_from(&["hello"]);
        let selection = Selection::new((2, 0));
        assert!(selection.is_empty());
        assert_eq!(selection.extract_text(&buffer), None);
    }

    #[test]
    fn selection_past_buffer_bounds_is_clamped_not_panicking() {
        let buffer = buffer_from(&["short"]);
        let selection = Selection {
            anchor: (0, 0),
            cursor: (200, 50),
        };
        // Must not panic; exact text isn't the point here, just safety.
        let _ = selection.extract_text(&buffer);
    }
}
