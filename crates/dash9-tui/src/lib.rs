//! ratatui/crossterm frontend: renders `dash9_core::Frame` values as
//! panels in a grid (timeseries, gauge, table, stat).
//!
//! See `docs/architecture/rendering.md` for the projection pipeline
//! and dependency boundaries this crate must keep. `chart` and
//! `theme` are the presentation-model and semantic-color layers;
//! Ratatui draw code is added on top of them, never the other way
//! around.

pub mod chart;
pub mod theme;
