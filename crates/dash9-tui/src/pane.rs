//! Shared pane chrome (`docs/specs/open.md` Section G.2): every bordered
//! pane in `dash9 open` follows the same border-embedded convention —
//! **name** top-left, **status** top-right, **key hint** bottom-left,
//! bottom-right reserved for future use — built once here so every draw
//! function (panel charts, Layout outlines, the detail pane, ...) gets it
//! uniformly instead of reinventing its own title/border styling. Reduces
//! visual density the way `loremesh-tui` does it: one line of chrome per
//! border already there for the title, not an extra row per pane.
//!
//! Color carries emphasis, never the only signal (Mechanism 4, same rule
//! `theme.rs`/`chart.rs` already follow): the focused pane's border and
//! name brighten, but its name text and status text are still legible
//! and meaningful with color stripped entirely.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders};

use crate::theme;

/// Builds one pane's border: `name` (top-left, bold+bright when
/// `focused`), an optional `status` (top-right, pre-colored by the
/// caller — this module doesn't know what a status *means*, only where
/// it renders; owned `String` since status text is typically `format!`-ed
/// on the fly), and an optional `hint` (bottom-left, muted — callers only
/// pass one when it's actually actionable right now, e.g. a hint for an
/// unfocused panel would describe keys that don't do anything).
pub fn pane_block(
    name: &str,
    focused: bool,
    status: Option<(String, Color)>,
    hint: Option<&str>,
) -> Block<'static> {
    let border_color = if focused { theme::FOCUS } else { theme::MUTED };
    let name_style = if focused {
        Style::default()
            .fg(theme::FOCUS)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(theme::TEXT)
    };

    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title_top(Line::styled(name.to_string(), name_style).left_aligned());

    if let Some((status_text, color)) = status {
        block =
            block.title_top(Line::styled(status_text, Style::default().fg(color)).right_aligned());
    }
    if let Some(hint_text) = hint {
        block = block.title_bottom(
            Line::styled(hint_text.to_string(), Style::default().fg(theme::MUTED)).left_aligned(),
        );
    }

    block
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn render(block: Block<'static>, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| f.render_widget(block, f.area())).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect()
    }

    #[test]
    fn name_only_draws_without_panicking() {
        let block = pane_block("CPU Usage", false, None, None);
        let content = render(block, 30, 5);
        assert!(content.contains("CPU Usage"));
    }

    #[test]
    fn status_and_hint_both_render() {
        let block = pane_block(
            "CPU Usage",
            true,
            Some(("● ok".to_string(), theme::SUCCESS)),
            Some("space detail"),
        );
        let content = render(block, 40, 5);
        assert!(content.contains("CPU Usage"));
        assert!(content.contains("ok"));
        assert!(content.contains("space detail"));
    }

    #[test]
    fn no_hint_when_none_is_passed() {
        let block = pane_block("CPU Usage", false, None, None);
        let content = render(block, 40, 5);
        assert!(!content.contains("detail"));
    }

    /// Regression test: a focused panel used to always show its hint
    /// (`hint: focused.then_some(PANEL_HINT)` in `draw.rs`), including
    /// while the command box was capturing keystrokes — so the border
    /// claimed `i` would open detail when it was actually typed as a
    /// literal character instead. Callers now compute hint eligibility
    /// separately from focus (`draw.rs`'s `show_hint` — `focused &&
    /// !editing`), so a focused-but-not-hinted pane (the state while
    /// editing) is a real, supported combination this module must render
    /// correctly: bright/focused chrome, no hint claiming otherwise.
    #[test]
    fn focused_pane_with_no_hint_still_shows_focused_chrome_but_no_hint_text() {
        let block = pane_block("CPU Usage", true, None, None);
        let content = render(block, 40, 5);
        assert!(content.contains("CPU Usage"));
        assert!(
            !content.contains("detail"),
            "focused alone must not imply the hint is shown: {content}"
        );
    }

    #[test]
    fn degenerate_tiny_area_draws_without_panicking() {
        let block = pane_block(
            "CPU",
            true,
            Some(("x".to_string(), theme::SUCCESS)),
            Some("i"),
        );
        let _ = render(block, 3, 1);
    }
}
