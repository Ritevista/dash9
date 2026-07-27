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
/// clipped to it rather than panicking or drawing off-screen. Equivalent
/// to `grid_layout_scrolled(area, panels, 0)` — kept as its own name
/// since "no scroll" is by far the common case (Layout/Focus zoom levels,
/// `dash9 demo`, every existing caller before Grid viewport paging
/// existed, `docs/specs/session-layout.md` Section A.2).
pub fn grid_layout(area: Rect, panels: &[GridSpec]) -> Vec<Rect> {
    grid_layout_scrolled(area, panels, 0)
}

/// Like [`grid_layout`], but the whole panel grid is first shifted up by
/// `scroll` content row-units before clipping to `area` — the Grid zoom
/// level's viewport paging (`docs/specs/session-layout.md` Section A.2/C).
/// A panel whose content range falls entirely outside `[scroll, scroll +
/// area.height)` gets a zero-size `Rect`, the same "clipped panel → zero
/// area" convention `grid_layout` already used for panels below the
/// terminal; a panel straddling the viewport edge is partially clipped
/// (its border may be cut off) rather than hidden outright or shown in
/// full past the edge — v1 pages by row-units, not whole-panel snapping.
pub fn grid_layout_scrolled(area: Rect, panels: &[GridSpec], scroll: u16) -> Vec<Rect> {
    let columns = grid_columns();
    panels
        .iter()
        .map(|grid| {
            place_scrolled(
                area,
                relative_rect(area.width, grid, columns, ROW_UNIT_HEIGHT),
                scroll,
            )
        })
        .collect()
}

/// The Layout zoom level's variant (`docs/specs/session-layout.md` Section
/// A.1): every panel visible at once, all scaled to fit `area` — instead of
/// the fixed `ROW_UNIT_HEIGHT`, the row-unit height is computed dynamically
/// so the tallest stack of panels fits exactly within `area.height`, trading
/// data legibility (individual panels may end up too short for even a
/// chart's text fallback) for completeness — *when that's actually
/// possible*. The row-unit height can shrink all the way down to `1`
/// (still positioning every panel), so the real limit isn't legibility at
/// all: once `total_row_units` itself exceeds `area.height`, no row-unit
/// height (not even `1`) makes everything fit — every panel's *absolute*
/// position still overflows the terminal, so panels past whatever fits
/// clip to nothing with no way to page to them. Real, confirmed case: a
/// 124-panel Grafana import spanning thousands of row-units (`docs/specs/
/// grafana-dashboards.md` Section H's collapsed-row rebasing) — "every
/// panel is already visible" silently stops being true, reported live as
/// arrow-key navigation "getting lost." Past that point this falls back to
/// the exact same fixed-height, `scroll`-driven clipping
/// [`grid_layout_scrolled`] already uses for Grid (reusing all of Grid's
/// paging: `next_grid_row_boundary`/`prev_grid_row_boundary`/
/// `grid_viewport_height_for_whole_rows`), rather than degrading into a
/// division that still doesn't fit. [`total_row_units`] is exposed so a
/// caller sizing the viewport ahead of time (`open.rs`'s `pane_heights`)
/// can make this identical fit-or-scroll decision itself — the two scales
/// (raw row-units here vs. `content_height`'s `ROW_UNIT_HEIGHT`-multiplied
/// terminal rows) don't correspond, so a caller that used `content_height`
/// to decide would disagree with this function about which path a given
/// frame takes.
pub fn grid_layout_fit(area: Rect, panels: &[GridSpec], scroll: u16) -> Vec<Rect> {
    let columns = grid_columns();
    let total_row_units = total_row_units(panels);
    if total_row_units > area.height {
        return grid_layout_scrolled(area, panels, scroll);
    }
    let row_unit_height = (area.height / total_row_units).max(1);
    panels
        .iter()
        .map(|grid| {
            let rel = relative_rect(area.width, grid, columns, row_unit_height);
            let placed = Rect {
                x: area.x.saturating_add(rel.x),
                y: area.y.saturating_add(rel.y),
                width: rel.width,
                height: rel.height,
            };
            placed.intersection(area)
        })
        .collect()
}

fn grid_columns() -> u16 {
    u16::try_from(GRID_COLUMNS).unwrap_or(12).max(1)
}

/// The tallest stack of panels, in raw row-units — not yet scaled by any
/// row-unit height. This is the exact number [`grid_layout_fit`] compares
/// against `area.height` to decide whether it can shrink everything down to
/// fit (down to its row-unit-height floor of `1`) or must fall back to
/// scrolling. Exposed so callers that need to size a viewport *before*
/// calling `grid_layout_fit` (`open.rs`'s `pane_heights`) can make the
/// identical fit-or-scroll decision, rather than the two silently
/// disagreeing about which rendering path a given frame will actually take.
pub fn total_row_units(panels: &[GridSpec]) -> u16 {
    panels
        .iter()
        .map(|g| u16::try_from(g.row.saturating_add(g.h)).unwrap_or(u16::MAX))
        .max()
        .unwrap_or(1)
        .max(1)
}

/// A column boundary's offset from the left edge of a `width`-wide area,
/// proportionally distributed — not `col * (width / columns)`, which
/// truncates on every multiplication and can leave several terminal
/// columns on the right entirely unused by any panel (visible, on a
/// terminal whose width doesn't divide evenly by `GRID_COLUMNS`, as a
/// full-width panel's border falling a few columns short of the
/// command bar's below it — reported live). Computing each boundary
/// independently from the full width means a panel spanning every
/// column always reaches exactly `width`, with any remainder distributed
/// across interior boundaries instead of lost off the right edge.
fn column_offset(width: u16, columns: u16, col: u16) -> u16 {
    let col = col.min(columns);
    let offset = (u32::from(width) * u32::from(col)) / u32::from(columns);
    u16::try_from(offset).unwrap_or(u16::MAX)
}

/// A panel's `Rect` relative to its content area's own origin (`x = 0, y =
/// 0`) rather than any particular render `area` — shared by every layout
/// variant above (`grid_layout`/`grid_layout_scrolled` via `ROW_UNIT_HEIGHT`,
/// `grid_layout_fit` via its own dynamic row-unit height), each of which
/// differs only in how it places/clips this relative rect against a real
/// `area`.
fn relative_rect(area_width: u16, grid: &GridSpec, columns: u16, row_unit_height: u16) -> Rect {
    let clamp = |v: u32| u16::try_from(v).unwrap_or(u16::MAX);
    let col_start = clamp(grid.col);
    let col_end = col_start.saturating_add(clamp(grid.w));
    let left = column_offset(area_width, columns, col_start);
    let right = column_offset(area_width, columns, col_end);
    Rect {
        x: left,
        y: clamp(grid.row).saturating_mul(row_unit_height),
        width: right.saturating_sub(left),
        height: clamp(grid.h).saturating_mul(row_unit_height),
    }
}

/// Places a content-relative `rect` (see [`relative_rect`]) into `area`,
/// shifted up by `scroll` content row-units first. A rect entirely above
/// or below the resulting viewport collapses to zero size instead of
/// wrapping/underflowing; one straddling an edge is clipped to the
/// visible portion, matching `Rect::intersection`'s existing clipping
/// convention (`grid_layout`'s doc comment) for the `scroll == 0` case.
fn place_scrolled(area: Rect, rect: Rect, scroll: u16) -> Rect {
    let content_top = rect.y;
    let content_bottom = rect.y.saturating_add(rect.height);
    let viewport_bottom = scroll.saturating_add(area.height);
    if content_bottom <= scroll || content_top >= viewport_bottom {
        return Rect {
            x: area.x.saturating_add(rect.x),
            y: area.y,
            width: 0,
            height: 0,
        };
    }
    let visible_top = content_top.max(scroll);
    let visible_bottom = content_bottom.min(viewport_bottom);
    Rect {
        x: area.x.saturating_add(rect.x),
        y: area.y.saturating_add(visible_top - scroll),
        width: rect.width,
        height: visible_bottom.saturating_sub(visible_top),
    }
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

/// How far `grid_scroll` (`ShellState`, content row-units) can go before
/// the viewport would show nothing but empty space below the last panel —
/// `0` when everything already fits in `viewport_height`. Callers `.min()`
/// the user's requested scroll against this rather than `ShellState`
/// tracking it itself (`ShellState` has no notion of terminal size; see
/// its own module docs).
pub fn max_grid_scroll(panels: &[GridSpec], viewport_height: u16) -> u16 {
    content_height(panels).saturating_sub(viewport_height)
}

/// Merges `panels` into visual row bands by vertical overlap — a
/// standard interval-merge over each panel's `[top, bottom)` content
/// range (content-relative, same row-unit-scaled terminal rows
/// [`grid_layout_scrolled`]'s `scroll` uses), sorted by `top`. Two
/// panels belong to the same band iff their content ranges overlap,
/// directly or transitively through a third panel. Real dashboards
/// (unlike the hand-aligned `node-overview.toml` worked example) don't
/// line every panel in a visual row up on exactly the same `row`
/// value — small panels of different heights sharing a row commonly
/// start a couple of row-units apart (seen live on a real 124-panel
/// Grafana import: 58 distinct `row` values, merging into 47 real
/// bands — not the ~16 a human looking at it would call "rows," since
/// one Grafana row *section* commonly stacks more than one band of
/// panels, but far fewer than 58). Feeds both
/// [`next_grid_row_boundary`]/[`prev_grid_row_boundary`] (paging) and
/// [`grid_viewport_height_for_whole_rows`] (render clipping) — the two
/// need the same bands, just different projections of them (tops
/// only, vs. full `[top, bottom)` ranges).
fn row_bands(panels: &[GridSpec]) -> Vec<(u16, u16)> {
    let mut spans: Vec<(u16, u16)> = panels
        .iter()
        .map(|grid| {
            let top = u16::try_from(grid.row)
                .unwrap_or(u16::MAX)
                .saturating_mul(ROW_UNIT_HEIGHT);
            let bottom = top.saturating_add(
                u16::try_from(grid.h)
                    .unwrap_or(u16::MAX)
                    .saturating_mul(ROW_UNIT_HEIGHT),
            );
            (top, bottom)
        })
        .collect();
    spans.sort_unstable();

    let mut bands: Vec<(u16, u16)> = Vec::new();
    for (top, bottom) in spans {
        match bands.last_mut() {
            Some((_, end)) if top < *end => *end = (*end).max(bottom),
            _ => bands.push((top, bottom)),
        }
    }
    bands
}

/// Every row band's own top (see [`row_bands`]) — the set of places
/// [`next_grid_row_boundary`]/[`prev_grid_row_boundary`] can land
/// `grid_scroll` on, so `PageUp`/`PageDown` in Grid zoom always show a
/// complete row instead of clipping one mid-height.
fn row_boundaries(panels: &[GridSpec]) -> Vec<u16> {
    row_bands(panels).into_iter().map(|(top, _)| top).collect()
}

/// `PageDown`'s target in Grid zoom: the nearest row boundary strictly
/// past `scroll` — the top of the next full row of panels, not a
/// fixed pixel step (`docs/specs/session-layout.md` Section A.2).
/// `scroll` itself (a no-op) when already on or past the last row's
/// top; the composition root separately clamps the result against
/// [`max_grid_scroll`], same as every other `grid_scroll` write.
pub fn next_grid_row_boundary(panels: &[GridSpec], scroll: u16) -> u16 {
    row_boundaries(panels)
        .into_iter()
        .find(|&top| top > scroll)
        .unwrap_or(scroll)
}

/// `PageUp`'s target in Grid zoom: the nearest row boundary strictly
/// before `scroll` — `0` (the first row) if `scroll` is already at or
/// before it. See [`next_grid_row_boundary`].
pub fn prev_grid_row_boundary(panels: &[GridSpec], scroll: u16) -> u16 {
    row_boundaries(panels)
        .into_iter()
        .rev()
        .find(|&top| top < scroll)
        .unwrap_or(0)
}

/// Shrinks a Grid-zoom viewport that would otherwise clip a row band
/// partway through at the bottom edge — the actual bug behind
/// "stretches and contracts": even after paging lands `scroll` exactly
/// on a band's top ([`next_grid_row_boundary`]), a raw
/// terminal-space-driven `available` height (`open.rs`'s `pane_heights`)
/// has no reason to be an exact multiple of the visible bands' heights,
/// so the *next* band commonly pokes a few rows into the bottom of the
/// viewport — just enough for a chart to try to render into a
/// near-zero-height `Rect` and come out looking squashed/distorted,
/// confirmed live. This returns the largest height `<= available` that
/// never lets a *new* band (one whose top falls inside the window,
/// i.e. not the band `scroll` is already inside) start without also
/// fully fitting; the leftover space becomes blank, not another
/// clipped chart. The band `scroll` is already inside is left alone —
/// scrolling into the middle of an over-tall band and showing only
/// what fits is the existing, correct, unrelated behavior
/// ([`ensure_visible`]'s "pinned to top" case has the same shape).
pub fn grid_viewport_height_for_whole_rows(
    panels: &[GridSpec],
    scroll: u16,
    available: u16,
) -> u16 {
    let viewport_end = scroll.saturating_add(available);
    let mut height = available;
    for (top, bottom) in row_bands(panels) {
        if top > scroll && top < viewport_end {
            let relative_bottom = bottom.saturating_sub(scroll);
            if relative_bottom > available {
                height = height.min(top.saturating_sub(scroll));
            }
        }
    }
    height
}

/// The first panel (by index) not entirely scrolled above `scroll` —
/// the topmost panel visible once the Grid viewport starts at
/// `scroll`. `None` for an empty panel list. Used to move
/// `focused_panel` to match the viewport after `PageUp`/`PageDown`
/// (`open.rs`'s `snap_grid_scroll_to_row`) — the reverse of what
/// [`ensure_visible`] already does for `Tab`/`Shift+Tab` (move the
/// viewport to match focus). Without this, paging past the focused
/// panel leaves it focused-but-offscreen: no panel on screen shows
/// the focus highlight, and the detail pane (`i`) shows a completely
/// different panel than whatever is actually visible — confirmed
/// live.
pub fn panel_at_scroll(panels: &[GridSpec], scroll: u16) -> Option<usize> {
    panels.iter().enumerate().find_map(|(index, grid)| {
        let bottom = u16::try_from(grid.row.saturating_add(grid.h))
            .unwrap_or(u16::MAX)
            .saturating_mul(ROW_UNIT_HEIGHT);
        (bottom > scroll).then_some(index)
    })
}

/// The focused panel's content-relative row range (`(top, bottom)`, in the
/// same units [`grid_layout_scrolled`]'s `scroll` uses) — `None` when
/// `index` is out of bounds (e.g. an empty dashboard). Feeds
/// [`ensure_visible`].
pub fn panel_content_range(panels: &[GridSpec], index: usize) -> Option<(u16, u16)> {
    let grid = panels.get(index)?;
    let top = u16::try_from(grid.row)
        .unwrap_or(u16::MAX)
        .saturating_mul(ROW_UNIT_HEIGHT);
    let bottom = top.saturating_add(
        u16::try_from(grid.h)
            .unwrap_or(u16::MAX)
            .saturating_mul(ROW_UNIT_HEIGHT),
    );
    Some((top, bottom))
}

/// Nudges `scroll` the minimum amount so `range` (a panel's content row
/// range, [`panel_content_range`]) is fully within the visible
/// `[scroll, scroll + viewport_height)` window — `Tab`/`Shift+Tab`
/// "keeping the newly-focused panel visible" (`docs/specs/session-layout.md`
/// Section B). Computed at render time from the real viewport height, not
/// stored back into `ShellState` (see its `grid_scroll` field docs). A
/// panel taller than the viewport itself is pinned to its top — there's
/// no better answer without sub-panel scrolling, which is out of scope
/// (Section C, Focus's reserved-but-unbuilt `PageUp`/`PageDown`).
pub fn ensure_visible(scroll: u16, range: (u16, u16), viewport_height: u16) -> u16 {
    let (top, bottom) = range;
    if bottom.saturating_sub(top) >= viewport_height {
        return top;
    }
    if top < scroll {
        top
    } else if bottom > scroll.saturating_add(viewport_height) {
        bottom.saturating_sub(viewport_height)
    } else {
        scroll
    }
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
                w: 12,
                h: 4,
            }, // CPU Usage
            GridSpec {
                row: 0,
                col: 12,
                w: 6,
                h: 4,
            }, // Load Average
            GridSpec {
                row: 0,
                col: 18,
                w: 6,
                h: 4,
            }, // Disk Free %
            GridSpec {
                row: 4,
                col: 0,
                w: 24,
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
    fn full_width_panel_reaches_the_right_edge_even_when_width_does_not_divide_evenly_by_24() {
        // 160 / 24 truncates to 6, so the old `col * (width /
        // columns)` math gave a full-width panel only 144 columns —
        // 16 short of the area's actual 160, visible live as the
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
            w: 24,
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
            w: 24,
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
            w: 24,
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
            w: 12,
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

    #[test]
    fn grid_layout_scrolled_at_zero_matches_grid_layout() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 10,
        };
        let grids = node_overview_grids();
        assert_eq!(
            grid_layout_scrolled(area, &grids, 0),
            grid_layout(area, &grids)
        );
    }

    #[test]
    fn grid_layout_scrolled_shifts_panels_up_and_hides_ones_scrolled_past() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 24,
        };
        let grids = node_overview_grids();
        // Row 4's panel starts at content y=24; scrolling by 24 should
        // bring it fully into view at the top of the viewport, and push
        // row 0's panels (content y 0..24) entirely above it.
        let rects = grid_layout_scrolled(area, &grids, 24);
        assert_eq!(rects[0].height, 0, "row 0 panel scrolled fully out of view");
        assert_eq!(
            rects[3],
            Rect {
                x: 0,
                y: 0,
                width: 120,
                height: 24
            },
            "row 4 panel now sits at the top of the viewport, fully visible"
        );
    }

    #[test]
    fn grid_layout_scrolled_partially_clips_a_panel_straddling_the_viewport_edge() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 20,
        };
        let grids = node_overview_grids();
        // Scrolling by 10 leaves 14 rows of row-0's panels (24 tall)
        // visible, clipped at the top rather than hidden or overflowing.
        let rects = grid_layout_scrolled(area, &grids, 10);
        assert_eq!(
            rects[0],
            Rect {
                x: 0,
                y: 0,
                width: 60,
                height: 14
            }
        );
    }

    #[test]
    fn grid_layout_fit_scales_row_unit_height_down_to_fit_the_given_area() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 16,
        };
        let grids = node_overview_grids(); // tallest stack: row 4 + h 4 = 8 units
        let rects = grid_layout_fit(area, &grids, 0);
        // 16 / 8 = row-unit height 2, so row 4's panel (row=4,h=4) sits at
        // y=8, height=8 — reaching exactly the bottom of the area, nothing
        // clipped, unlike grid_layout at the same area.
        assert_eq!(rects[3].y, 8);
        assert_eq!(rects[3].height, 8);
        for rect in &rects {
            assert!(rect.height > 0, "grid_layout_fit never hides a panel");
        }
    }

    #[test]
    fn grid_layout_fit_falls_back_to_scrolling_instead_of_a_degenerate_shrink() {
        // total_row_units(8) > area.height(2) — even the row-unit-height
        // floor of 1 can't make this fit (needs 8 rows, has 2), so this
        // must take the grid_layout_scrolled fallback (same fixed
        // ROW_UNIT_HEIGHT Grid uses) rather than attempt a division that
        // still wouldn't fit.
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 2,
        };
        let grids = node_overview_grids();
        let rects = grid_layout_fit(area, &grids, 0);
        assert_eq!(rects, grid_layout_scrolled(area, &grids, 0));
    }

    #[test]
    fn grid_layout_fit_shows_every_panel_when_the_area_exactly_matches_row_units() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 8, // exactly node_overview_grids' 8 total row-units
        };
        let rects = grid_layout_fit(area, &node_overview_grids(), 0);
        assert!(
            rects.iter().all(|r| r.height > 0),
            "row-unit height floors at 1, so an area matching the unit count fits everything"
        );
    }

    #[test]
    fn grid_layout_fit_falls_back_to_scrolling_when_total_row_units_exceed_area_height() {
        // A real, confirmed case: a dashboard whose content genuinely
        // can't be shrunk into the given area (grafana-dashboards.md
        // Section H's collapsed-row rebasing can push row values into
        // the thousands) must page instead of clipping everything past
        // the first few panels with no way to reach the rest.
        let grids = [GridSpec {
            row: 0,
            col: 0,
            w: 24,
            h: 1000,
        }];
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 40,
        };
        let rects = grid_layout_fit(area, &grids, 0);
        assert_eq!(rects, grid_layout_scrolled(area, &grids, 0));
    }

    #[test]
    fn grid_layout_fit_of_no_panels_is_empty() {
        let area = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 40,
        };
        assert!(grid_layout_fit(area, &[], 0).is_empty());
    }

    #[test]
    fn max_grid_scroll_is_zero_when_content_already_fits() {
        assert_eq!(max_grid_scroll(&node_overview_grids(), 48), 0);
        assert_eq!(max_grid_scroll(&node_overview_grids(), 100), 0);
    }

    #[test]
    fn max_grid_scroll_is_the_overflow_when_content_does_not_fit() {
        assert_eq!(max_grid_scroll(&node_overview_grids(), 20), 28); // 48 - 20
    }

    #[test]
    fn next_grid_row_boundary_finds_the_next_distinct_row_top() {
        // node_overview_grids' rows are 0 and 4 (row-units) -> content
        // y 0 and 24 (ROW_UNIT_HEIGHT 6). Three panels share row 0, so
        // that duplicate must not produce a spurious extra boundary.
        let grids = node_overview_grids();
        assert_eq!(next_grid_row_boundary(&grids, 0), 24);
        assert_eq!(
            next_grid_row_boundary(&grids, 10),
            24,
            "skips past a mid-row scroll to the next full row"
        );
    }

    #[test]
    fn next_grid_row_boundary_is_a_no_op_past_the_last_row() {
        let grids = node_overview_grids();
        assert_eq!(next_grid_row_boundary(&grids, 24), 24);
        assert_eq!(next_grid_row_boundary(&grids, 100), 100);
    }

    #[test]
    fn prev_grid_row_boundary_finds_the_previous_distinct_row_top() {
        let grids = node_overview_grids();
        assert_eq!(prev_grid_row_boundary(&grids, 24), 0);
        assert_eq!(
            prev_grid_row_boundary(&grids, 30),
            24,
            "skips back past a mid-row scroll to that row's own top"
        );
    }

    #[test]
    fn prev_grid_row_boundary_floors_at_zero() {
        let grids = node_overview_grids();
        assert_eq!(prev_grid_row_boundary(&grids, 0), 0);
        assert_eq!(prev_grid_row_boundary(&grids, 10), 0);
    }

    #[test]
    fn row_boundaries_of_an_empty_panel_list_never_move_scroll() {
        assert_eq!(next_grid_row_boundary(&[], 5), 5);
        assert_eq!(prev_grid_row_boundary(&[], 5), 0);
    }

    #[test]
    fn grid_viewport_height_shrinks_to_hide_a_band_that_would_be_cut_at_the_bottom() {
        // node_overview_grids: band 1 is [0,24) (row 0's three panels),
        // band 2 is [24,48) (row 4's full-width panel). available=30
        // lets band 2 start poking in at relative offset 24, but only
        // 6 rows of it (24..30) would show -- exactly the squashed-
        // chart bug reported live. The viewport must shrink to 24,
        // hiding band 2 entirely rather than clipping it.
        let grids = node_overview_grids();
        assert_eq!(grid_viewport_height_for_whole_rows(&grids, 0, 30), 24);
    }

    #[test]
    fn grid_viewport_height_is_unchanged_when_every_visible_band_fits_exactly() {
        let grids = node_overview_grids();
        assert_eq!(grid_viewport_height_for_whole_rows(&grids, 0, 48), 48);
    }

    #[test]
    fn grid_viewport_height_leaves_the_band_scroll_is_already_inside_alone() {
        // available=20 doesn't even fully fit band 1 (0..24) itself --
        // that's the existing "scrolled into an over-tall band, show
        // what fits" case (`ensure_visible`'s "pinned to top"), not a
        // new band poking through the bottom, so no further shrinking.
        let grids = node_overview_grids();
        assert_eq!(grid_viewport_height_for_whole_rows(&grids, 0, 20), 20);
    }

    #[test]
    fn grid_viewport_height_of_no_panels_is_unchanged() {
        assert_eq!(grid_viewport_height_for_whole_rows(&[], 0, 20), 20);
    }

    #[test]
    fn overlapping_panels_at_slightly_different_rows_merge_into_one_boundary() {
        // Mirrors a real Grafana import: small panels sharing a
        // visual row don't all start at the exact same `row` — they
        // just overlap. All three of these belong to the same band
        // (row-units: [1,9), [3,7), [6,10) all touch), so PageDown
        // must jump straight past the whole band, not stop partway
        // through it at row 3 or row 6.
        let grids = vec![
            GridSpec {
                row: 1,
                col: 0,
                w: 8,
                h: 8,
            },
            GridSpec {
                row: 3,
                col: 8,
                w: 8,
                h: 4,
            },
            GridSpec {
                row: 6,
                col: 16,
                w: 8,
                h: 4,
            },
            GridSpec {
                row: 12,
                col: 0,
                w: 24,
                h: 4,
            }, // a clearly separate second band
        ];
        assert_eq!(
            next_grid_row_boundary(&grids, 0),
            6,
            "from before the first band, the boundary is simply that band's own top"
        );
        assert_eq!(
            next_grid_row_boundary(&grids, 6),
            72,
            "from inside the first band, jumps straight to the second band's top \
             (row 12 * ROW_UNIT_HEIGHT 6) — not row 3 or row 6, which are still part of the first band"
        );
        assert_eq!(prev_grid_row_boundary(&grids, 72), 6, "row 1 * 6");
    }

    #[test]
    fn panel_at_scroll_finds_the_topmost_visible_panel() {
        // node_overview_grids: panels 0-2 are row 0 (content [0,24)),
        // panel 3 is row 4 (content [24,48)).
        let grids = node_overview_grids();
        assert_eq!(
            panel_at_scroll(&grids, 0),
            Some(0),
            "at the very top, the first panel"
        );
        assert_eq!(
            panel_at_scroll(&grids, 24),
            Some(3),
            "scrolled to row 4's boundary, the row-4 panel is topmost"
        );
    }

    #[test]
    fn panel_at_scroll_skips_panels_fully_scrolled_past() {
        let grids = node_overview_grids();
        // Scrolled to 10 (mid row-0): row-0 panels (bottom=24) are
        // still partially visible, so they're still "topmost."
        assert_eq!(panel_at_scroll(&grids, 10), Some(0));
    }

    #[test]
    fn panel_at_scroll_of_no_panels_is_none() {
        assert_eq!(panel_at_scroll(&[], 0), None);
    }

    #[test]
    fn panel_at_scroll_past_every_panel_is_none() {
        let grids = node_overview_grids();
        assert_eq!(panel_at_scroll(&grids, 1000), None);
    }

    #[test]
    fn panel_content_range_reports_content_relative_row_units() {
        let grids = node_overview_grids();
        assert_eq!(panel_content_range(&grids, 0), Some((0, 24))); // row 0, h 4
        assert_eq!(panel_content_range(&grids, 3), Some((24, 48))); // row 4, h 4
    }

    #[test]
    fn panel_content_range_out_of_bounds_is_none() {
        assert_eq!(panel_content_range(&[], 0), None);
    }

    #[test]
    fn ensure_visible_is_a_no_op_when_already_fully_visible() {
        assert_eq!(ensure_visible(0, (0, 24), 24), 0);
        assert_eq!(ensure_visible(10, (24, 48), 48), 10);
    }

    #[test]
    fn ensure_visible_scrolls_up_to_reveal_a_panel_above_the_viewport() {
        // range (0, 20) is shorter than the 24-row viewport, so this
        // exercises "scroll up to the panel's top," not the separate
        // taller-than-viewport pin.
        assert_eq!(ensure_visible(24, (0, 20), 24), 0);
    }

    #[test]
    fn ensure_visible_scrolls_down_to_reveal_a_panel_below_the_viewport() {
        // range (30, 50) is shorter than the 24-row viewport; the minimal
        // scroll that brings its bottom into view is bottom - viewport.
        assert_eq!(ensure_visible(0, (30, 50), 24), 26);
    }

    #[test]
    fn ensure_visible_pins_to_the_top_when_the_panel_is_taller_than_the_viewport() {
        assert_eq!(ensure_visible(100, (24, 72), 20), 24);
    }
}
