# Fixture-replay datasource — docs/specs/fixture-datasource.md

A second `Datasource` implementation (`docs/adr/0005-datasource-port.md`)
alongside `dash9-prom`: instead of an HTTP call, it replays a
previously-captured `Frame` from disk. This is how you develop, demo,
or test a dashboard fully offline against *real* metrics you captured
once — distinct from `dash9 demo`'s synthetic data (generated, not
real) and from the `docker-compose.yml` live-Prometheus path (real,
but needs the network up).

Status: **Proposed** — nothing below is implemented. Prerequisites:
`SPEC.md` Section A (the `Frame` model and its JSON shape, Section A.5
— this spec introduces no new wire format, only a new place that shape
is read from), `docs/adr/0005-datasource-port.md`.

## Contents

- [A. Capture](#a-capture)
- [B. Replay](#b-replay)
- [C. Fixture matching](#c-fixture-matching)
- [D. Non-goals](#d-non-goals)

---

## A. Capture

`dash9 test <path> --save-fixtures <dir>` runs the exact same headless
execution `dash9 test` already does (`SPEC.md` C.3: load, validate,
run every panel's query once, live) — no new query-execution code path
— with one side effect: each panel's real, successfully-returned
`Frame` is serialized (`SPEC.md` A.5's existing JSON shape, unchanged)
and written to `<dir>/<fixture-key>.json` (Section C). A panel whose
query fails is not written — a fixture directory only ever contains
real, successful results, never a captured error. `--save-fixtures`
changes nothing about `dash9 test`'s pass/fail verdict or exit codes
(`SPEC.md` C.3) — capture is purely additive alongside the existing
validation run.

Live capture from inside `dash9 open` (archiving whatever a panel
returns during an interactive session) is explicitly deferred — see
Section D. Start with the batch path above; it covers "run once
against real infra, save what came back" without touching the
interactive session at all.

## B. Replay

A new `[[datasources]]` entry, `type = "fixture"`, whose `url` is a
path to a fixture directory (Section A) instead of an HTTP endpoint.
`dash9 open`/`dash9 test` against a dashboard using a fixture
datasource need no network at all — every query resolves by reading a
file. Everything downstream of the datasource boundary (`Frame`,
`dash9-tui`'s rendering, `dash9 test`'s pass/fail logic) is unchanged;
a fixture-backed panel is indistinguishable from a live one once its
`Frame` exists in memory — same invariant `dash9-prom` already
guarantees at its own boundary (`SPEC.md` A.1: adapters normalize to
`Frame` and never leak their native shape past it).

No new crate: unlike `dash9-prom` (a real HTTP client, third-party
dependencies, worth its own crate), a fixture adapter is local file
I/O only — it lives directly in the `dash9` binary crate alongside the
other concrete adapters/wiring (`docs/architecture/rendering.md`'s
review-map entry for `crates/dash9/src/main.rs`), not as a new
workspace member.

**This is the trigger `docs/adr/0005-datasource-port.md` named in
advance.** That ADR accepted a non-object-safe `Datasource` trait
(native async-fn-in-trait, no `dyn Datasource`) specifically because
v0.1 only ever held one concrete adapter type at a time
(`HashMap<String, PrometheusDatasource>`); it explicitly flagged
"revisit if/when a second datasource type needs to coexist behind one
dynamically-dispatched handle" as the condition that would force a
different shape. A `fixture` datasource alongside `dash9-prom` is
exactly that condition — `HashMap<String, PrometheusDatasource>` can't
hold both concrete types. The two ways out are the same two ADR 0005
already named: an enum (`enum ConcreteDatasource { Prometheus(...),
Fixture(...) }`, keeping native async-fn-in-trait, dispatched by
match) or making `Datasource` object-safe (boxed futures, `dyn
Datasource`, giving up native async-fn-in-trait). This spec doesn't
pick one — that choice belongs in a superseding ADR once this spec
moves past Proposed, not decided as a side effect of writing the
spec.

## C. Fixture matching

A fixture file is looked up by the panel's exact query text (`SPEC.md`
B.2's raw-tail `q` string, unmodified — the same string already stored
verbatim in `Frame.meta.query`), hashed into a filename. If a panel's
query has no matching fixture (never captured, or changed since
capture), that is reported as a query-execution failure (`E106`,
`SPEC.md` B.5) — "no fixture for this query" — the same error shape a
live datasource being unreachable already produces, not a silent
fallback to stale or placeholder data. This matches `SPEC.md` A.4's
existing rule that a failed query is always `Result::Err`, never a
synthesized empty `Frame`.

## D. Non-goals

- **No live capture from `dash9 open`.** Only the batch `dash9 test
  --save-fixtures` path (Section A) captures. Interactive-session
  capture is a plausible later extension, not designed here.
- **No fuzzy/partial query matching.** A fixture matches one exact
  query string or it doesn't match at all (Section C) — no "close
  enough" heuristic that could silently serve the wrong data.
- **No fixture expiry or staleness warnings.** A fixture is valid
  until its query text changes; there's no timestamp-based "this
  fixture is N days old" signal in this phase.
- **No fixture editing tools.** A fixture file is `Frame` JSON
  (`SPEC.md` A.5) — editing one by hand is possible but unsupported
  and untooled, same as hand-editing any other fixture in this
  codebase's existing test suites.
