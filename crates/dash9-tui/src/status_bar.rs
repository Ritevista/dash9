//! Top status bar: dashboard/datasource summary and, when built with
//! the `assist` feature and its config loaded successfully, AI status
//! (model, on/off, connectivity, token usage).
//! Pure rendering, plain data in — `dash9-tui` has no `dash9-assist`
//! dependency, so [`AssistStatusLine`] is a plain struct the
//! composition root fills in from its own locally-tracked state, not
//! from any `dash9-assist` type directly (same reasoning as
//! `crate::shell`'s `CommandHandler`: the trait/data boundary here
//! never needs to know assist exists).

use std::fmt::Write as _;

use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::theme;

/// Derived from whether any panel's last query result was an error —
/// not a separate live connectivity probe (panels already surface
/// their own errors inline; this is just an at-a-glance summary of
/// the same information, not a new signal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasourceHealth {
    Healthy,
    Degraded,
    /// No panel has reported a result yet (e.g. still loading).
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssistStatusLine {
    pub model: String,
    pub enabled: bool,
    /// Short label: "idle" | "waiting" | "error: ...". Plain text, not
    /// an enum — this is a display-only mirror the composition root
    /// tracks locally (see module docs), not a reusable domain type.
    pub connectivity: String,
    pub tokens_sent: u32,
    pub tokens_received: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatusBarModel {
    pub title: String,
    pub panel_count: usize,
    pub datasource_summary: String,
    pub health: DatasourceHealth,
    /// `None` when not built with the `assist` feature, or its config
    /// couldn't be loaded — the AI segment is omitted entirely rather
    /// than shown disabled, since there's nothing configured to toggle.
    pub assist: Option<AssistStatusLine>,
}

pub fn draw_status_bar(frame: &mut Frame, area: Rect, model: &StatusBarModel) {
    if area.height == 0 {
        return;
    }
    let marker = match model.health {
        DatasourceHealth::Healthy => "●",
        DatasourceHealth::Degraded => "▲",
        DatasourceHealth::Unknown => "○",
    };

    let mut line = format!(
        " {title} │ {count} panel{plural} │ {marker} {ds} ",
        title = model.title,
        count = model.panel_count,
        plural = if model.panel_count == 1 { "" } else { "s" },
        ds = model.datasource_summary,
    );
    if let Some(assist) = &model.assist {
        let _ = write!(
            line,
            "│ AI: {state} │ {model_name} │ {connectivity} │ ↑{sent}↓{received} tok ",
            state = if assist.enabled { "on" } else { "off" },
            model_name = assist.model,
            connectivity = assist.connectivity,
            sent = assist.tokens_sent,
            received = assist.tokens_received,
        );
    }

    // The marker glyph alone carries the health meaning (Mechanism 4);
    // no per-segment color here keeps this first pass simple.
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(theme::TEXT)),
        area,
    );
}

/// One line below the main status bar: the active zoom level plus that
/// region's own key hint (`docs/specs/session-layout.md` Section D — "per
/// bordered region" hints, so discoverability doesn't live solely in
/// `/help`). Kept as its own small model/draw pair rather than folded into
/// [`StatusBarModel`]/[`draw_status_bar`] — the zoom concept has nothing
/// to do with datasource health or AI status, the two things that bar
/// already tracks, and every future zoom-bar tweak (e.g. Grid's "panels
/// X-Y of Z" paging indicator, computed by the composition root from real
/// rect data) stays isolated from that already-busy line.
#[derive(Debug, Clone, PartialEq)]
pub struct ZoomBarModel {
    /// e.g. `"Grid"`, `"Layout"`, `"Focus: chart"`, `"Focus: inspect"`.
    pub zoom_label: String,
    /// The active region's key hint (`shell::zoom_hint`), with the
    /// composition root's own "panels X-Y of Z" suffix appended when Grid
    /// is truncated — this module renders whatever text it's given, same
    /// "pure rendering" split every other `dash9-tui` draw module keeps.
    pub hint: String,
}

pub fn draw_zoom_bar(frame: &mut Frame, area: Rect, model: &ZoomBarModel) {
    if area.height == 0 {
        return;
    }
    let line = format!(" [{}] {} ", model.zoom_label, model.hint);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(theme::MUTED)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn backend(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).unwrap()
    }

    fn base_model() -> StatusBarModel {
        StatusBarModel {
            title: "Node Overview".to_string(),
            panel_count: 4,
            datasource_summary: "1 datasource (prom)".to_string(),
            health: DatasourceHealth::Healthy,
            assist: None,
        }
    }

    #[test]
    fn draws_without_assist_line_without_panicking() {
        let mut terminal = backend(120, 3);
        terminal
            .draw(|f| draw_status_bar(f, f.area(), &base_model()))
            .unwrap();
    }

    #[test]
    fn draws_with_assist_line_without_panicking() {
        let mut model = base_model();
        model.assist = Some(AssistStatusLine {
            model: "gemini-flash".to_string(),
            enabled: true,
            connectivity: "waiting".to_string(),
            tokens_sent: 123,
            tokens_received: 45,
        });
        let mut terminal = backend(160, 3);
        terminal
            .draw(|f| draw_status_bar(f, f.area(), &model))
            .unwrap();
    }

    #[test]
    fn degraded_and_unknown_health_draw_without_panicking() {
        let mut terminal = backend(120, 3);
        for health in [DatasourceHealth::Degraded, DatasourceHealth::Unknown] {
            let mut model = base_model();
            model.health = health;
            terminal
                .draw(|f| draw_status_bar(f, f.area(), &model))
                .unwrap();
        }
    }

    #[test]
    fn zero_height_area_draws_without_panicking() {
        let mut terminal = backend(120, 3);
        terminal
            .draw(|f| draw_status_bar(f, Rect::new(0, 0, 120, 0), &base_model()))
            .unwrap();
    }

    #[test]
    fn zoom_bar_draws_without_panicking() {
        let model = ZoomBarModel {
            zoom_label: "Grid".to_string(),
            hint: "PageUp/PageDown page panels · +/- zoom · space detail".to_string(),
        };
        let mut terminal = backend(120, 1);
        terminal
            .draw(|f| draw_zoom_bar(f, f.area(), &model))
            .unwrap();
    }

    #[test]
    fn zoom_bar_zero_height_area_draws_without_panicking() {
        let model = ZoomBarModel {
            zoom_label: "Layout".to_string(),
            hint: "+ back to grid".to_string(),
        };
        let mut terminal = backend(120, 1);
        terminal
            .draw(|f| draw_zoom_bar(f, Rect::new(0, 0, 120, 0), &model))
            .unwrap();
    }
}
