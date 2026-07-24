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
pub mod draw;
pub mod export;
pub mod layout;
pub mod shell;
pub mod status_bar;
pub mod theme;

pub use command_bar::draw_command_bar;
pub use draw::{draw_chart, draw_gauge, draw_stat, draw_table, series_as_table};
pub use export::{table_for_export, table_to_csv, table_to_markdown, ExportFormat};
pub use layout::{content_height, grid_layout};
pub use shell::{
    help_text, parse_shell_input, CommandHandler, CommandResponse, ShellInput, ShellState,
};
pub use status_bar::{draw_status_bar, AssistStatusLine, DatasourceHealth, StatusBarModel};
