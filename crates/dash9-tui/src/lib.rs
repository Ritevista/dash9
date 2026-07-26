//! ratatui/crossterm frontend: renders `dash9_core::Frame` values as
//! panels in a grid (timeseries, gauge, table, stat).
//!
//! See `docs/architecture/rendering.md` for the projection pipeline
//! and dependency boundaries this crate must keep. `chart` and
//! `theme` are the presentation-model and semantic-color layers;
//! Ratatui draw code is added on top of them, never the other way
//! around.

pub mod chart;
pub mod command_bar;
pub mod detail_view;
pub mod draw;
pub mod export;
pub mod layout;
pub mod output;
pub mod pane;
pub mod shell;
pub mod status_bar;
pub mod theme;

pub use command_bar::{
    command_bar_height, draw_command_bar, log_height, LogFocus, MAX_LOG_HEIGHT, MIN_LOG_HEIGHT,
};
pub use detail_view::{detail_height, draw_panel_detail, PanelDetail};
pub use draw::{
    draw_chart, draw_gauge, draw_panel_outline, draw_stat, draw_table, series_as_table, PANEL_HINT,
};
pub use export::{table_for_export, table_to_csv, table_to_markdown, ExportFormat};
pub use layout::{
    content_height, ensure_visible, grid_layout, grid_layout_fit, grid_layout_scrolled,
    max_grid_scroll, panel_content_range,
};
pub use output::{
    draw_output, max_output_scroll, output_height, MAX_OUTPUT_HEIGHT, MIN_OUTPUT_HEIGHT,
};
pub use pane::pane_block;
pub use shell::{
    help_text, parse_shell_input, zoom_hint, CommandHandler, CommandResponse, Region, ShellInput,
    ShellState, Zoom,
};
pub use status_bar::{
    draw_status_bar, draw_zoom_bar, AssistStatusLine, DatasourceHealth, StatusBarModel,
    ZoomBarModel,
};
