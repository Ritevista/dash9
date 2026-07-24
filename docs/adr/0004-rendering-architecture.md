# ADR 0004: Rendering architecture — presentation models separated from Ratatui

- Status: Accepted
- Date: 2026-07-19

## Context
A chart renderer that mixes data, layout math, and terminal widget calls together is hard to test (needs a real terminal) and hard to keep deterministic (needed for `dash9 test` output and future recorded demos).

## Decision
Follow a one-way projection pipeline: `Frame` (canonical, `dash9-core`) → `ChartModel` (presentation-agnostic projection: series selection, downsampling to terminal width, threshold evaluation — `dash9-tui::chart`, zero Ratatui imports) → a Ratatui widget (draw only, `dash9-tui::draw`). `ChartModel` stores data, labels, thresholds, and semantic status (`Severity`) only, never a `ratatui::style::Color`; the mapping from a series index or a `Severity` to a concrete color happens only in `dash9-tui::theme`, at draw time. Every panel type has a compact, deterministic text renderer with no Ratatui dependency (`ChartModel::render_text()`), used for narrow terminals, `dash9 test`-adjacent output, and any future export path — it formats timestamps in UTC rather than local time specifically so it stays reproducible across machines and CI runners. Interactive view state (zoom, selected series) lives in a separate `ChartViewState` type, applied during projection, and never leaks into `Frame` or anything exported. Full rationale, the dependency diagram, and the theme table live in `docs/architecture/rendering.md`.

## Consequences
`ChartModel` and `theme` are unit-testable with no terminal at all (see `crates/dash9-tui/src/chart.rs`'s and `draw.rs`'s test modules, the latter using `ratatui::backend::TestBackend`). Color is never load-bearing: a `Severity` carries its own marker glyph and label text independent of any color mapping. The cost is one more type in the pipeline (`ChartModel` alongside `Frame`) and a discipline requirement — a future panel type must not be tempted to reach for a `ratatui::style::Color` inside its model just because it's convenient.
