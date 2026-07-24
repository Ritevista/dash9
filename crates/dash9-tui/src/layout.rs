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
    panels
        .iter()
        .map(|grid| panel_rect(area, grid, columns))
        .collect()
}

/// A column boundary's absolute x, proportionally distributed across
/// `area`'s width — not `col * (area.width / columns)`, which
/// truncates on every multiplication and can leave several terminal
/// columns on the right entirely unused by any panel (visible, on a
/// terminal whose width doesn't divide evenly by `GRID_COLUMNS`, as a
/// full-width panel's border falling a few columns short of the
/// command bar's below it — reported live). Computing each boundary
/// independently from the full width means a panel spanning every
/// column always reaches exactly `area.x + area.width`, with any
/// remainder distributed across interior boundaries instead of lost
/// off the right edge.
fn column_x(area: Rect, columns: u16, col: u16) -> u16 {
    let col = col.min(columns);
    let offset = (u32::from(area.width) * u32::from(col)) / u32::from(columns);
    area.x
        .saturating_add(u16::try_from(offset).unwrap_or(u16::MAX))
}

fn panel_rect(area: Rect, grid: &GridSpec, columns: u16) -> Rect {
    let clamp = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    let col_start = clamp(grid.col);
    let col_end = col_start.saturating_add(clamp(grid.w));
    let left = column_x(area, columns, col_start);
    let right = column_x(area, columns, col_end);
    let raw = Rect {
        x: left,
        y: area
            .y
            .saturating_add(clamp(grid.row).saturating_mul(ROW_UNIT_HEIGHT)),
        width: right.saturating_sub(left),
        height: clamp(grid.h).saturating_mul(ROW_UNIT_HEIGHT),
    };
    raw.intersection(area)
}

/// How many terminal rows the panel grid actually needs (the tallest
/// panel's `row + h`, in `ROW_UNIT_HEIGHT` units) — for a caller that
/// wants the grid area sized to its content instead of stretched to
/// fill whatever space it's given. Without this, a grid area sized via
/// a `Min(0)` layout constraint on a terminal taller than the
/// dashboard's content leaves a dead, unrendered gap below the last
/// panel row (nothing draws there, since `grid_layout` positions
/// panels by absolute grid units, not by however much area it's
/// handed). `0` for an empty panel list.
pub fn content_height(panels: &[GridSpec]) -> u16 {
    panels
        .iter()
        .map(|grid| {
            u16::try_from(grid.row.saturating_add(grid.h))
                .unwrap_or(u16::MAX)
                .saturating_mul(ROW_UNIT_HEIGHT)
        })
        .max()
        .unwrap_or(0)
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
    fn full_width_panel_reaches_the_right_edge_even_when_width_does_not_divide_evenly_by_12() {
        // 160 / 12 truncates to 13, so the old `col * (width /
        // columns)` math gave a full-width panel only 156 columns —
        // 4 short of the area's actual 160, visible live as the
        // panel's border falling short of the log/command bar below
        // it (which spans the full area, not a grid column count).
        let area = Rect {
            x: 0,
            y: 0,
            width: 160,
            height: 40,
        };
        let grids = vec![GridSpec {
            row: 0,
            col: 0,
            w: 12,
            h: 4,
        }];
        let rects = grid_layout(area, &grids);
        assert_eq!(rects[0].width, 160);
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
    fn content_height_is_the_tallest_panels_row_plus_h() {
        assert_eq!(content_height(&node_overview_grids()), 48); // (row 4 + h 4) * 6
    }

    #[test]
    fn content_height_of_no_panels_is_zero() {
        assert_eq!(content_height(&[]), 0);
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
