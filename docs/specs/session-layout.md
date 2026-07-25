# Session layout: Layout/Grid/Focus views and contextual keys — docs/specs/session-layout.md

Extends `docs/specs/open.md` for dashboards too large to render at
once — the common case once real, imported Grafana dashboards
(`docs/specs/grafana-dashboards.md`) start arriving with 15-30+ panels
instead of the 4-panel example this codebase was originally designed
against. Introduces three zoom levels for the main area, makes
navigation keys contextual to whichever region is active, and phases
in `/save png`.

Status: **Proposed** — nothing in this document is implemented.
`docs/specs/open.md` remains the accurate description of what's
shipped today; this spec supersedes its Section E (keybindings) and
Section I (`/save png`'s "not implemented" stance) once built, and
`open.md` gets updated to match at that point, not before.

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
3. **Focus** — one panel, full-pane. This is `open.md` Section G's
   existing detail view, generalized: `i` still means "toggle detail"
   exactly as it does today (backward compatible, no existing test
   changes meaning), but Focus now has two sub-views instead of one —
   **chart** (the panel's chart, just bigger, no config/data) and
   **inspect** (today's config + raw data table). Entering Focus via
   `i` lands on inspect (matches today exactly); entering via `+`
   (Section B) lands on chart. Once in Focus, `i` toggles between the
   two sub-views regardless of which one you entered through.

## B. Zoom-level keys

Editor/browser convention, not a new invention: the three levels
(Section A) sit on one line, Layout ↔ Grid ↔ Focus, and `-`/`_` and
`+`/`=` always move one step along it — `-` toward Layout, `+` toward
Focus (on the currently focused panel, landing on the chart sub-view),
from *either* neighboring level, not just from Grid. At the two ends,
the key that would overshoot is a no-op: `-` at Layout does nothing
(already outermost), `+` at Focus does nothing (already innermost).
This is what makes Section D's "Layout shows `+` (back to Grid)" hint
correct — `+` is genuinely valid from Layout, not just from Grid.
`Esc` is the separate "go home" shortcut: it steps back one level
toward Grid specifically — Focus → Grid, or Layout → Grid — the same
incremental-undo shape `Esc` already has today (cancel input, *then*
close detail view, `open.md` Section E); Grid is the fixed "home"
level, so `Esc` at Grid is a no-op, unchanged from today. (`Esc` from
Layout and `+` from Layout both land on Grid — that overlap is
intentional, not a redundancy to remove: `Esc` is "go home" from
anywhere, `+`/`-` are "one step" from wherever you are.) `Tab`/
`Shift+Tab` continue to move panel focus within Grid or Focus exactly
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
| Focus, not editing | No-op for v1 — reserved for scrolling a single panel's own long content (e.g. a large table in the inspect sub-view); not built now, flagged so it isn't lost |

This is the general rule the whole session follows going forward, not
a one-off carve-out for this one key: a region-specific keymap, with
each region also owning its own hint text (Section D) — the two are
the same underlying mechanism (a region knows its own keys; showing
them and acting on them are the same data).

## D. Per-pane shortcut hints

Each bordered region gets a one-line footer or title-suffix listing
only its own relevant keys, instead of everything living solely in
`/help`. Mechanically this is the existing `command_bar_hint`
mechanism (`open.md`'s command box already does exactly this) applied
to every other region: Grid's border shows its paging/zoom keys, Focus
shows `i` (toggle sub-view) and `Esc`/`-` (back out), Layout shows `+`
(back to Grid). `/help` remains the full reference; these are the
"what can I press right here" complement, not a replacement.

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
