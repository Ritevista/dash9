# Grafana dashboard interoperability — docs/specs/grafana-dashboards.md

dash9 becomes a **terminal editor for real Grafana dashboard JSON**:
consume an exported Grafana dashboard, edit/debug/test it against a
live Prometheus datasource from the terminal, use `dash9-assist`
(`docs/specs/assist.md`) to improve it, and generate Grafana-compatible
JSON back out — so the result pastes straight back into Grafana. dash9
is not becoming a Grafana replacement: no dashboard *serving*, no
alerting engine, no multi-user access, no rendering-as-a-service. The
goal is a fast, scriptable, testable way to author and iterate on the
dashboards that ultimately run in Grafana, not to compete with it.

This supersedes one line of `SPEC.md` Section D ("No Grafana JSON
dashboard import or export") for the phase this spec covers — that
line was accurate for Phase 1 (v0.1) and remains an accurate
description of what v0.1 shipped; it does not describe this phase.

Status: **Partially implemented** — the *import* path (Sections C/D's
field mapping, Section F's grid migration and format detection) is
built and grounded against a real dashboard (`crates/dash9-core/src/
grafana.rs`: Grafana.com "Node Exporter Full", ID 1860, schemaVersion
41). Losslessly preserving unmapped fields for re-export (Section A's
round-trip guarantee, `dash9 dash save` writing Grafana JSON back out)
is **not** built — this phase only ever reads Grafana JSON, never
writes it. Prerequisites: `SPEC.md` (the TOML schema and `Frame` model
this maps onto), `docs/specs/open.md` (the session a Grafana-sourced
dashboard runs inside once loaded, including the `--prometheus-url`
flag this phase added), `docs/specs/assist.md` (how AI enhancement
plugs in).

## Contents

- [A. Round-trip principle](#a-round-trip-principle)
- [B. Scope: Prometheus panels only](#b-scope-prometheus-panels-only)
- [C. The Grafana JSON model dash9 reads](#c-the-grafana-json-model-dash9-reads)
- [D. Field mapping](#d-field-mapping)
- [E. Template variables](#e-template-variables)
- [F. Open design questions](#f-open-design-questions)
- [G. Non-goals](#g-non-goals)
- [H. Import implementation notes (what real dashboards needed)](#h-import-implementation-notes-what-real-dashboards-needed)

---

## A. Round-trip principle

**A Grafana dashboard dash9 doesn't fully understand must still survive
a round trip unchanged.** This is the load-bearing constraint on
everything below: real dashboards have panel types dash9 doesn't
render (heatmap, state timeline, node graph...), datasources dash9
doesn't query (Loki, InfluxDB, CloudWatch...), and display options
dash9 has no model for (per-panel color overrides, custom legends,
annotations). If import silently drops anything it doesn't recognize,
"edit and generate" becomes "edit and corrupt" the moment a real,
non-trivial dashboard goes through it — unacceptable for a tool whose
whole premise is being trusted with your actual Grafana JSON.

So: every field dash9 doesn't actively map is carried through as
opaque JSON, attached to the panel/dashboard it came from, and
re-emitted verbatim on export. dash9 only ever *adds or changes* the
fields it actually understands (query, thresholds, grid position for
Prometheus panels); it never *deletes* a field it doesn't recognize.
A panel dash9 can't execute (Section B) is still preserved, still
positioned in the grid, just not queryable or testable from inside
dash9.

## B. Scope: Prometheus panels only

dash9 still only executes queries against Prometheus (`SPEC.md`
Section D: "No datasources beyond Prometheus" — unchanged, still true).
On import:

- A panel whose `datasource.type` is `"prometheus"` maps fully (Section
  D) and is live, queryable, testable, editable inside dash9.
- A panel with any other datasource type is preserved (Section A) and
  shown in the panel grid as present-but-inert — title and position
  visible, no query execution. `dash9 test` reports it, rather than
  silently omitting it, but **exactly how is not decided by this spec**
  — `SPEC.md` C.3 / `docs/adr/0006-dash9-test-pure-verdict.md` define
  only `PASS`/`FAIL` and a 0/1/2 exit code today; there is no `SKIP`
  concept anywhere in `PanelCheckResult`. Adding one is a `SPEC.md` C.3
  amendment in its own right (does a dashboard with only `PASS` and the
  new outcome exit `0`? does the new outcome ever count toward exit
  `1`?) — out of scope to decide as a side effect of this spec. Until
  that amendment lands, treat a non-Prometheus panel in `dash9 test` as
  reported-but-excluded from the pass/fail verdict, and pin down the
  real shape before implementing this bullet.

This means "import a real Grafana dashboard" mostly works today even
for mixed-datasource dashboards; you get full editing power over the
Prometheus panels and lossless passthrough of everything else.

## C. The Grafana JSON model dash9 reads

The relevant subset of Grafana's dashboard JSON (Classic model,
Grafana 8+; see Section F for older exports):

```json
{
  "uid": "abc123",
  "title": "Node Overview",
  "schemaVersion": 39,
  "refresh": "30s",
  "time": { "from": "now-1h", "to": "now" },
  "panels": [
    {
      "id": 2,
      "type": "timeseries",
      "title": "CPU Usage",
      "gridPos": { "x": 0, "y": 0, "w": 12, "h": 8 },
      "datasource": { "type": "prometheus", "uid": "prom" },
      "targets": [
        { "refId": "A", "expr": "rate(node_cpu_seconds_total{mode=\"user\"}[5m])" }
      ],
      "fieldConfig": {
        "defaults": {
          "thresholds": {
            "mode": "absolute",
            "steps": [
              { "color": "green", "value": null },
              { "color": "yellow", "value": 0.75 },
              { "color": "red", "value": 0.9 }
            ]
          }
        }
      }
    }
  ],
  "templating": { "list": [] }
}
```

`datasource` on both a panel and an individual target can be either
this `{type, uid}` object (Grafana 8+) or a bare string naming the
datasource (pre-8 exports still circulate) — dash9 accepts both.

## D. Field mapping

| Grafana field | dash9 field | Notes |
|---|---|---|
| `title` | `DashboardMeta.title` | direct |
| `refresh` | `DashboardMeta.refresh` | Grafana's `"30s"`/`"1m"`/`"5m"` already matches `SPEC.md` B.4's duration grammar for common cases; `""`/`false` (Grafana's "off") maps to `RefreshInterval::Off` |
| `time.from`/`time.to` | `DashboardMeta.default_range` | only the common `"now-<duration>"`/`"now"` shape maps to a single `default_range` duration; an absolute or non-`now`-relative range has no dash9 equivalent and is rejected at import with a clear error, not silently approximated |
| `panels[].title` | `PanelSpec.title` | direct |
| `panels[].type` | `PanelSpec.type_` | mapped where dash9 has an equivalent (`timeseries`→`timeseries`, `gauge`→`gauge`, `table`→`table`, `stat`→`stat`); anything else preserved per Section A, not executed |
| `panels[].gridPos` | `PanelSpec.grid` | direct, 1:1 (Section F — `GRID_COLUMNS` moves from 12 to 24 to match Grafana exactly, decided below) |
| `panels[].datasource` (or `targets[0].datasource`) | `PanelSpec.datasource` | resolved to a dash9 `[[datasources]]` entry by matching `uid`; a Grafana datasource dash9 hasn't been told a URL for (all it has is a `uid`, not a queryable endpoint) prompts for one on first import rather than guessing |
| `panels[].targets[0].expr` | `PanelSpec.query` | direct — this is already dash9's raw-tail `q` string (`SPEC.md` B.2), verbatim, no rewriting |
| `panels[].targets[1..]` (multiple queries per panel) | — | dash9's schema is one query per panel (`SPEC.md` C.1); a multi-target panel imports its first target live and preserves the rest per Section A, unqueried — flagged, not merged or dropped |
| `fieldConfig.defaults.thresholds.steps` | `[[panels.thresholds]]` | each non-null step becomes a `ValidatedThreshold`; Grafana steps have no `name`, only a `color`, so the color name becomes the threshold's `name` (`"yellow"`, `"red"`) unless that collides, in which case `step-N`; `op` is always `gte` — Grafana's ascending-steps-take-the-highest-match model has no `lt`/`lte` equivalent to invert to |

## E. Template variables

`SPEC.md` Section D's "no query templating/variables" non-goal is
**not** reversed by this spec. `templating.list[]` variables are
resolved to their `current.value` at import time and substituted into
every `$variable` reference in every `expr`, producing a plain static
query — the same non-templated string dash9 has always executed. This
keeps every existing invariant (`q` is raw-tail and opaque to
`dash9-core`, `SPEC.md` B.2) true without inventing a variable-runtime
inside dash9. The cost: switching a variable's value is a Grafana-side
operation, not something you can do live inside a dash9 session yet.
The *unresolved* `templating.list` block itself is preserved per
Section A, so export doesn't destroy the variables Grafana will still
want.

## F. Open design questions

### Decided

**Grid columns move from 12 to 24, to match Grafana exactly.** `SPEC.md`
C.1 / `dash9_core::GRID_COLUMNS` fixed dash9's grid at 12 columns for
Phase 1, when nothing outside dash9 itself needed to agree with that
number. Staying at 12 means every import/export scales `x`/`w` by ½,
which is lossless only when every Grafana value happens to be even —
an odd-width panel (nothing stops one; Grafana doesn't enforce even
widths) rounds on import, and that rounding is not recoverable on
export: there's no way to tell afterward whether the original `x` was
6 or 7 once it's stored as `col: 3`. A dashboard that's imported and
immediately re-exported with zero edits would still drift. Moving to
24 makes every position round-trip exactly, and costs only a
mechanical migration: `GRID_COLUMNS` is a single named constant
threaded through validation (`dashboard.rs`) and rendering
(`dash9-tui/src/layout.rs`) — no scattered hardcoded `12`s — so the
actual work is doubling `examples/node-overview.toml`'s existing
`grid.w`/`grid.col` values (and anyone else's dash9-native TOML files)
to keep their current visual layout, plus re-deriving `layout.rs`'s
terminal-width test fixtures for the new column count. 24 is a strict
superset of what 12 could express — every existing 12-column layout
still fits exactly (just double every number) — so nothing that works
today gets harder, only odd-width positions that were previously
unrepresentable become possible.

**Where Grafana JSON lives in the CLI surface, decided:** no new
verbs. `dash9 open <path>` / `dash9 test <path>` and the in-session
`dash save`/`dash open <path>` grammar (`SPEC.md` B.3) detect the
format from the file itself (`.json` vs `.toml`, content-sniffed if
the extension is ambiguous) and just handle either — the grammar
verb's arity and meaning don't change, only what file shapes its one
path argument can target, so this doesn't touch the append-only
guarantee (B.1). The session remembers which format it was loaded
from: a Grafana-loaded session's bare `dash save` (or `dash save` back
to the same path) overwrites it *as Grafana JSON*, preserved-opaque-
fields and all (Section A) — the "load it, tweak it, save it back"
loop stays a two-step loop, with no TOML file appearing in the middle
that nobody asked for. Naming a different extension (`dash save
foo.toml` from a Grafana-loaded session) is a deliberate, one-way
format conversion, not the default path — and since TOML has no pocket
for what dash9 doesn't model (non-Prometheus panels, alerting rules,
annotations), that direction is genuinely lossy and has to say so out
loud (e.g. "3 panels using unsupported datasources were dropped
converting to TOML") rather than silently drop them. Section A's
losslessness guarantee holds same-format-to-same-format; crossing
formats is a real, reported downgrade, not free.

### Still open

One decision this spec doesn't make unilaterally: **what does
"generate" mean for a dashboard authored from scratch in dash9** (not
imported from Grafana at all)? It needs a `uid`, `schemaVersion`, and
the other Grafana-only bookkeeping fields (Section C) synthesized from
nothing — worth deciding the defaults (e.g. `schemaVersion` pinned to
whichever version this spec is grounded against) before `dash9 dash
save` can emit valid Grafana JSON for a dashboard that never came from
Grafana.

## G. Non-goals

- **No execution of non-Prometheus panels.** Preserved (Section A),
  never queried. Adding a new datasource adapter is a separate,
  unrelated decision (`SPEC.md` D still applies).
- **No live template-variable switching.** Resolved once at import
  (Section E); the unresolved definition is preserved for export, not
  made interactive.
- **No Grafana alerting rules, annotations, or panel links.** Preserved
  per Section A if present; dash9 has no concept of any of them.
- **No Grafana API integration.** This spec is about the JSON file
  format only — pushing a dashboard to a running Grafana instance
  over its HTTP API, or pulling one down live, is a distinct,
  unstarted capability, not assumed here.

## H. Import implementation notes (what real dashboards needed)

Grounded against a real, non-trivial dashboard rather than only the
worked example in Section C — the public "Node Exporter Full"
(Grafana.com ID 1860). That surfaced real structure this spec's
earlier sections didn't anticipate; the decisions below are the
implementation's answers, recorded here since nothing above covers
them. Import-only (module docs, `crates/dash9-core/src/grafana.rs`) —
none of this exists on the export side, which isn't built yet.

- **Row panels.** Not mentioned anywhere above. An expanded row's
  children are flat siblings in `panels[]`, positioned by their own
  `gridPos`; a *collapsed* row's children are nested inside the row's
  own `panels[]` instead. Both are flattened into a plain panel list;
  the row entry itself carries no query and is dropped, not imported
  as a panel of its own.
- **A collapsed row's nested panels are not in the dashboard's shared
  coordinate space.** A worse surprise than the nesting itself: a
  collapsed row's `gridPos.y` values reflect wherever that row's
  panels sat the *last time it was expanded*, independent of every
  other row — not a continuation of the running layout. Confirmed on
  the real grounding dashboard: two unrelated collapsed rows both used
  `y` ranges around 21..480 and 733..982, genuinely overlapping.
  Importing every nested panel's raw `y` unchanged rendered multiple
  unrelated rows on top of each other — doubled/garbled chart output,
  confirmed live, initially misread as a rendering bug before tracing
  it back to the import step. Fixed by rebasing: each collapsed row's
  block is shifted down by just enough to start right after whatever
  was already placed (tracking a running "next available row" through
  the whole file in order), preserving every nested panel's position
  *relative to its own block* while eliminating the cross-block
  collision. A block that already lands past the running position (as
  most in-order-authored dashboards do) gets no shift at all.
- **The datasource `uid` can itself be a template variable**
  (`"uid": "${ds_prometheus}"`), unaddressed by Section D's "resolved
  by matching `uid`." Since dash9 only ever supports one datasource
  *type* (Prometheus), the uid's literal value — variable or not — is
  never actually matched; any panel with `datasource.type ==
  "prometheus"` resolves to the one Prometheus datasource dash9 was
  given at import.
- **Where that one Prometheus datasource's URL comes from**: Section D
  says dash9 "prompts for one on first import rather than guessing."
  Implemented as `dash9 open`/`dash9 test --prometheus-url <url>`
  (default `http://localhost:9090`, `docs/specs/open.md` Section A) —
  an explicit CLI input, not an interactive in-TUI dialog. Revisit if
  multi-datasource dashboards ever need more than one URL.
- **Grafana built-in variables** (`$__rate_interval`, `$__interval`,
  `$__range`, ...) never appear in `templating.list[]`, so Section E's
  "substitute `current.value`" rule has nothing to substitute — and on
  the dashboard this was grounded against, every single panel query
  used `$__rate_interval`. dash9 fills in exactly that one, using
  Grafana's own documented default (`max($__interval +
  scrape_interval, 4 * scrape_interval)` at Grafana's documented
  default `scrape_interval` of 15s → `1m`). Every other builtin is
  left unresolved like any other variable with no value — not guessed.
- **Variables exported with no saved value.** A dashboard shared on
  grafana.com (rather than exported live from a running Grafana
  instance) normally has `current: {}` for every variable — there is
  nothing to substitute, and Section E's non-goal (no live
  variable-runtime) means dash9 can't resolve one dynamically either.
  A panel whose query still contains an unresolved `$name` after
  substitution imports as **preserved but inert**
  (`ValidatedPanel::executable = false`, with a human-readable
  `inert_reason`) — visible, positioned, never queried. The same
  treatment Section B already specifies for a non-Prometheus panel,
  extended to this case. On the grounding dashboard, this is the
  common case, not the exception: with no pinned variable values, all
  124 panels import inert; pinning `job`/`node`/`nodename` to real
  values (as a live Grafana session would have) brings 120 of them to
  fully executable, live-querying panels — verified end to end against
  a real Prometheus + node_exporter stack.
- **`fieldConfig.defaults.thresholds`** has two shapes Section D's
  worked example didn't show, both real here: a step with no `value`
  key at all (not even `null`) is Grafana's "base" color for
  everything below the first real threshold, skipped rather than
  defaulted to `0`; and `mode: "percentage"` steps are relative to the
  panel's configured min/max, not absolute metric values — mapping
  those numbers straight across would produce a threshold that looks
  valid but compares on the wrong scale, so a non-`"absolute"` mode
  drops the panel's thresholds entirely rather than misapplying them.
  Separately: a threshold's `color` is a free-form CSS color, not
  always a named one (`rgba(245, 54, 54, 0.9)` is common) — only a
  plain word is used as the threshold's display name, anything else
  falls back to a generic `"threshold"`.
- **`dash9 test`'s pass/fail verdict** treats an inert panel as `SKIP`
  — reported, but excluded from the pass/fail verdict, the same
  resolution Section B already proposes for a non-Prometheus panel
  (no new `PanelCheckResult` variant; still deferred as its own
  SPEC.md C.3 amendment, per Section B).
- **`allow_empty` defaults to `true` for every imported panel** — the
  opposite of a TOML panel's strict default (SPEC.md C.1). Grafana JSON
  has no `allow_empty` field to map at all; this is a synthesized
  default, not a mapping. A hand-authored TOML panel's empty result
  usually signals something's actually wrong, so `false` is the right
  strict default there. A general-purpose community dashboard (Node
  Exporter Full, grounding this spec, is exactly this) covers many
  *optional* exporter collectors no single environment enables all of
  — e.g. `node_tcp_connection_states` needs node_exporter's `tcpstat`
  collector, off by default upstream. Failing `dash9 test` over a
  normal deployment difference, indistinguishable from a genuinely
  broken query under the strict default, is worse than the (rare) case
  of a real regression going unflagged.
