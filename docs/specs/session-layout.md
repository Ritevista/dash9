# Session layout: Layout/Grid/Focus views and contextual keys — docs/specs/session-layout.md

Extends `docs/specs/open.md` for dashboards too large to render at
once — the common case once real, imported Grafana dashboards
(`docs/specs/grafana-dashboards.md`) start arriving with 15-30+ panels
instead of the 4-panel example this codebase was originally designed
against. Introduces three zoom levels for the main area, makes
navigation keys contextual to whichever region is active, and phases
in `/save png`.

Status: **Partially implemented** — Sections A-D (the three zoom
levels, zoom-level keys, contextual `PageUp`/`PageDown`, and per-region
key hints) are built and covered by unit tests
(`crates/dash9-tui/src/{shell,layout,draw,status_bar,detail_view}.rs`)
plus manual smoke tests against a live session; `docs/specs/open.md`
Sections E and G now describe this shipped behavior directly. Section
A.3's original "Focus has chart/inspect sub-views" design was revised
after shipping — see that section's "Revised after shipping" note and
`open.md` Section G.1 — direct user feedback on the live build found
that replacing the whole main area to show detail hid every chart with
no direct way back; detail is now a separate, always-below pane,
decoupled from zoom entirely. Section E below (`/save png`, phased)
remains **Proposed** — deliberately deferred, since it needs a new
image/font-rasterization dependency this codebase doesn't have yet and
is a separable follow-up, not a prerequisite for the zoom-level work.

## Contents

- [A. The three zoom levels](#a-the-three-zoom-levels)
- [B. Zoom-level keys](#b-zoom-level-keys)
- [C. Contextual keys, generally](#c-contextual-keys-generally)
- [D. Per-pane shortcut hints](#d-per-pane-shortcut-hints)
- [E. `/save png`, phased](#e-save-png-phased)

---

## A. The three zoom levels

The main area (everything `open.md` Section G called "the panel grid
or the detail view") is really three distinct zoom levels, not two:

1. **Layout** — every panel in the dashboard, all at once, structure
   only. Trades data legibility for completeness: a panel too small to
   even hit the existing narrow-width text fallback (`chart.rs`'s
   deterministic degradation, `docs/architecture/rendering.md`) drops
   to title-and-border-only rather than clipping or overflowing. The
   point of this level is confirming the *arrangement* is right, not
   reading any panel's data. No panel is ever hidden here — that's
   what distinguishes it from Grid.
2. **Grid** (today's default, `open.md`'s existing behavior) — real
   charts at readable size. When more panel-rows exist than the
   terminal has height for, the viewport pages vertically (Section C)
   instead of clipping silently, with the same "you're not seeing
   everything" affordance the log already uses (`open.md` Section F's
   log retitles itself when scrolled; Grid gets the equivalent, e.g.
   `"panels 5-8 of 12 — PageDown for more"`).
3. **Focus** — one panel's chart, full-pane, nothing more. **Revised
   after shipping**: this section originally proposed Focus with two
   sub-views (`i`-toggled "chart" and "inspect," the latter being
   `open.md` Section G's pre-existing config+data detail view,
   generalized into a zoom sub-view). That shipped, then real usage
   immediately surfaced the problem the design review missed: entering
   the inspect sub-view replaced the *entire* main area, so a panel's
   chart(s) became completely invisible while inspecting one, with no
   affordance more direct than `Esc`/`-` to get back. The fix, per
   direct user feedback, decouples detail from zoom entirely: `i` now
   toggles a **separate pane below** the main grid/layout/focus area
   (`open.md` Section G.1) — the chart(s) stay visible and usable the
   whole time. `Zoom::Focus` still exists and still means exactly what
   this bullet's first sentence says (one panel's chart, enlarged, via
   `+`); it just no longer has sub-views, and `i` no longer touches
   zoom at all. See `open.md` Section G/G.1 for the shipped design.

## B. Zoom-level keys

Editor/browser convention, not a new invention: the three levels
(Section A) sit on one line, Layout ↔ Grid ↔ Focus, and `-`/`_` and
`+`/`=` always move one step along it — `-` toward Layout, `+` toward
Focus (the currently focused panel's chart, enlarged), from *either*
neighboring level, not just from Grid. At the two ends, the key that
would overshoot is a no-op: `-` at Layout does nothing (already
outermost), `+` at Focus does nothing (already innermost). This is
what makes Section D's "Layout shows `+` (back to Grid)" hint correct
— `+` is genuinely valid from Layout, not just from Grid. `Esc` is the
separate "go home" shortcut: it steps back one level toward Grid
specifically — Focus → Grid, or Layout → Grid — but only once the
detail pane (`open.md` Section G.1, independent of zoom — see Section
A.3's revision above) is closed; Esc closes that first if it's open,
the same layered shape Esc already had before zoom levels existed
(cancel input, *then* close detail). Grid is the fixed "home" level,
so `Esc` at Grid with detail already closed is a no-op. (`Esc` from
Layout and `+` from Layout both land on Grid — that overlap is
intentional, not a redundancy to remove: `Esc` is "go home" from
anywhere, `+`/`-` are "one step" from wherever you are.) `Tab`/
`Shift+Tab` continue to move panel focus within any zoom level exactly
as today, and now also scroll the Grid viewport to keep the
newly-focused panel visible — so casual navigation never requires
learning the paging key below at all.

## C. Contextual keys, generally

Keys are interpreted by whichever region is currently active, not
globally — the same "keep the number of keys small, keep behavior
predictable" reasoning that named this design. `PageUp`/`PageDown`
change meaning by active region:

| Active region | `PageUp`/`PageDown` effect |
|---|---|
| Command box (editing) | Scroll the log — unchanged from `open.md` Section E, still works while typing without touching the buffer |
| Grid, not editing | Page the panel viewport vertically (Section A.2) |
| Layout, not editing | No-op — nothing to page; every panel is already visible by definition |
| Focus, not editing | No-op for v1 — reserved for scrolling a single panel's own long content; not built now, flagged so it isn't lost. The detail pane's data table (`open.md` Section G.1) has the same unbuilt-scrolling gap, tracked there instead since it's no longer part of Focus. |

This is the general rule the whole session follows going forward, not
a one-off carve-out for this one key: a region-specific keymap, with
each region also owning its own hint text (Section D) — the two are
the same underlying mechanism (a region knows its own keys; showing
them and acting on them are the same data).

## D. Per-pane shortcut hints

**Revised after shipping**, same reason and same shape as Section
A.3's revision: this section originally proposed one hint per
*region* (Grid/Focus/Layout), each showing its whole key set
("Focus shows `i` (toggle sub-view) and `Esc`/`-`..."). Once Focus's
sub-views were removed (Section A.3), "toggle sub-view" stopped being
true, and the shipped design ended up richer than a region-level hint
line anyway — it's now a genuine per-**pane** convention, not just
per-region:

- The **zoom bar** (`open.md` Section H) is what this section's
  original per-region hint became: one line, region-level keys only
  (`PageUp`/`PageDown` paging, `Tab`/`1`-`9` selection, `+`/`-` zoom) —
  the things true of the *whole* active region, not any one panel.
- Each individual **pane's own border** (`open.md` Section G.2,
  `dash9_tui::pane::pane_block`) carries the genuinely pane-specific
  bit instead: bottom-left key hint (just `i` — the only truly
  per-panel action), top-right status, top-left name, all color-coded
  by focus state. This is more than "a footer listing keys" — the
  original ask here — it's a full per-pane chrome convention that
  subsumed this section's goal and then some.

`/help` remains the full reference either way; both the zoom bar and
each pane's own border are the "what can I press right here"
complement, not a replacement. See `open.md` Section G.2 for the
shipped design in full.

## E. `/save png`, phased

`open.md` Section I currently reports `png` as a recognized-but-
unavailable format. Phase 1 here makes it real without waiting for a
full chart rasterizer: rasterize the actual Ratatui `Buffer` — each
cell's character and style rendered onto a monospace-glyph canvas —
into a PNG. This captures whatever's currently on screen (Grid,
Layout, or Focus, whatever's active), the same technique terminal-
screenshot tools use. It's a complete, independently useful capability
on its own — sharing what a session currently looks like — not a
stepping-stone that gets thrown away.

**Phase 2, explicitly deferred, not built now:** a second renderer
consuming the same `ChartModel` (`docs/architecture/rendering.md`,
ADR 0004 — `ChartModel` already knows nothing about Ratatui
specifically, by design) through a real rasterizer (e.g. `plotters`)
to produce genuinely Grafana-quality chart images — smooth lines,
proper axes — rather than a monospace terminal capture. This is the
"exactly like Grafana" target; recorded here so the smaller Phase 1
doesn't get mistaken for the whole feature.
