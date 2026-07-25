# ADR 0009: Grafana dashboard JSON as a first-class, lossless format

- Status: Proposed
- Date: 2026-07-25

## Context

dash9's stated purpose shifted from "a TUI with its own dashboard
format" to "a terminal editor for the Grafana dashboards you actually
run" — consume, edit, test, AI-enhance, and regenerate real Grafana
dashboard JSON, without dash9 becoming a Grafana replacement (no
serving, no alerting, no multi-user). This directly supersedes one
line of `SPEC.md` Section D ("No Grafana JSON dashboard import or
export") for this phase; that line remains an accurate description of
what Phase 1 (v0.1) shipped. Full design detail lives in
`docs/specs/grafana-dashboards.md`; this ADR records only the durable
architectural boundary, ahead of implementation — same split ADR 0008
uses for `docs/specs/assist.md`.

## Decision

Grafana dashboard JSON becomes a format dash9 reads and writes
natively, chosen automatically from the file itself — no new CLI
verbs; `dash9 open`/`dash9 test` and the in-session `dash save`/`dash
open` grammar (ADR 0003's append-only verbs, unchanged arity) just
handle either format. A session remembers which format it was loaded
from; saving back to the same format is lossless by construction:
**any field dash9 doesn't actively map is carried through as opaque
JSON and re-emitted verbatim, never silently dropped** — this is the
load-bearing constraint, not an implementation detail, because a tool
whose whole premise is being trusted with someone's real production
dashboard cannot quietly corrupt the parts it doesn't understand.
Query execution stays Prometheus-only (`SPEC.md` D's other non-goal is
unchanged) — a panel using any other datasource is preserved and
positioned but never queried. dash9's grid moves from 12 columns to 24
to match Grafana's exactly, so panel position round-trips exactly
rather than rounding on every import/export.

## Consequences

A real, unmodified Grafana dashboard survives `dash9 open` → no edits
→ save unchanged, byte-for-bit on every field dash9 doesn't model —
the property that makes "edit the 5% you care about, trust the other
95% untouched" safe to promise. Converting *across* formats (a
Grafana-loaded session saved explicitly as `.toml`) is a deliberate,
reported, one-way downgrade — TOML has no pocket for what it doesn't
model, so that direction is honestly lossy, unlike same-format
round-trips. The cost: `GRID_COLUMNS` moving from 12 to 24 is a
breaking change to every existing dash9-native TOML file's
`grid.w`/`grid.col` values (mechanical migration, not a design risk —
see `docs/specs/grafana-dashboards.md` Section F for why), and the
opaque-passthrough requirement means every future dash9-native field
addition to the dashboard model must also define how it's represented
in the Grafana JSON shape, not just the TOML one.
