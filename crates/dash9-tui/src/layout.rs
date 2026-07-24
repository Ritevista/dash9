//! Grid layout: `[[panels]].grid` coordinates (SPEC.md C.1) → absolute
//! terminal `Rect`s. Pure math, no Ratatui terminal required to test.
//!
//! Each panel's `Rect` is computed independently from its own
//! `(row, col, w, h)` — there is no packing algorithm. Two grid specs
//! that overlap simply produce overlapping `Rect`s, matching SPEC.md
//! Section D's explicit non-goal of overlap validation.

use dash9_core::{GridSpec, GRID_COLUMNS};
use ratatui::layout::Rect;

/// Terminal rows per grid unit of panel height. Chosen so a
/// timeseries chart (axis, legend, a few plot rows) stays legible at
/// `h = 4`, the height every panel in `examples/node-overview.toml`
/// uses.
const ROW_UNIT_HEIGHT: u16 = 6;

/// Computes each panel's absolute `Rect` within `area`, in input
/// order. A panel positioned fully or partially outside `area` is
/// clipped to it (v1 has no scrolling) rather than panicking or
/// drawing off-screen.
pub fn grid_layout(area: Rect, panels: &[GridSpec]) -> Vec<Rect> {
    let columns = u16::try_from(GRID_COLUMNS).unwrap_or(12).max(1);
    let column_width = area.width / columns;
    panels
        .iter()
        .map(|grid| panel_rect(area, grid, column_width))
        .collect()
}

fn panel_rect(area: Rect, grid: &GridSpec, column_width: u16) -> Rect {
    let clamp = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    let raw = Rect {
        x: area
            .x
            .saturating_add(clamp(grid.col).saturating_mul(column_width)),
        y: area
            .y
            .saturating_add(clamp(grid.row).saturating_mul(ROW_UNIT_HEIGHT)),
        width: clamp(grid.w).saturating_mul(column_width),
        height: clamp(grid.h).saturating_mul(ROW_UNIT_HEIGHT),
    };
    raw.intersection(area)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact grid values from `examples/node-overview.toml` /
    /// SPEC.md C.2's worked example.
    fn node_overview_grids() -> Vec<GridSpec> {
        vec![
            GridSpec {
                row: 0,
                col: 0,
                w: 6,
                h: 4,
            }, // CPU Usage
            GridSpec {
                row: 0,
                col: 6,
                w: 3,
                h: 4,
            }, // Load Average
            GridSpec {
                row: 0,
                col: 9,
                w: 3,
                h: 4,
            }, // Disk Free %
            GridSpec {
                row: 4,
                col: 0,
                w: 12,
                h: 4,
            }, // Top Processes
        ]
    }

    #[test]
    fn positions_panels_side_by_side_on_a_wide_terminal() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 40,
        };
        let rects = grid_layout(area, &node_overview_grids());

        assert_eq!(
            rects[0],
            Rect {
                x: 0,
                y: 0,
                width: 60,
                height: 24
            }
        );
        assert_eq!(
            rects[1],
            Rect {
                x: 60,
                y: 0,
                width: 30,
                height: 24
            }
        );
        assert_eq!(
            rects[2],
            Rect {
                x: 90,
                y: 0,
                width: 30,
                height: 24
            }
        );
        // Row 4 * ROW_UNIT_HEIGHT(6) = y 24, full width.
        assert_eq!(
            rects[3],
            Rect {
                x: 0,
                y: 24,
                width: 120,
                height: 16
            }
        );
    }

    #[test]
    fn panel_extending_past_the_terminal_is_clipped_not_panicked() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 10,
        };
        let grids = vec![GridSpec {
            row: 0,
            col: 0,
            w: 12,
            h: 4,
        }];
        let rects = grid_layout(area, &grids);

        // Wants height 24 (h=4 * 6), but the terminal only has 10 rows.
        assert_eq!(
            rects[0],
            Rect {
                x: 0,
                y: 0,
                width: 120,
                height: 10
            }
        );
    }

    #[test]
    fn panel_positioned_entirely_below_the_terminal_has_zero_area() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 10,
        };
        let grids = vec![GridSpec {
            row: 4,
            col: 0,
            w: 12,
            h: 4,
        }];
        let rects = grid_layout(area, &grids);

        assert_eq!(rects[0].height, 0);
    }

    #[test]
    fn empty_panel_list_produces_no_rects() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 40,
        };
        assert!(grid_layout(area, &[]).is_empty());
    }

    #[test]
    fn offsets_by_the_area_origin() {
        let area = Rect {
            x: 5,
            y: 2,
            width: 120,
            height: 40,
        };
        let grids = vec![GridSpec {
            row: 0,
            col: 0,
            w: 6,
            h: 4,
        }];
        let rects = grid_layout(area, &grids);
        assert_eq!(
            rects[0],
            Rect {
                x: 5,
                y: 2,
                width: 60,
                height: 24
            }
        );
    }
}
