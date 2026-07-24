# ADR 0002: Frame as the single cross-datasource data model

- Status: Accepted
- Date: 2026-07-18

## Context
Every panel type, every renderer, and eventually every non-Prometheus datasource need one shared shape to agree on, or the TUI and the headless test runner would each need datasource-specific knowledge.

## Decision
`dash9-core` owns `Frame` — one of `Timeseries`, `InstantVector`, or `Table`, sharing a common `FrameMeta` envelope. Adapters normalize at their boundary and never leak their native response shape past it: `dash9-prom` converts Prometheus's fractional Unix-seconds timestamps to `i64` UTC milliseconds, and its own JSON structures, entirely inside `crates/dash9-prom/src/lib.rs`. Series are not forced onto a shared time grid — gap-filling policy belongs to a renderer, not the data model. There is exactly one definition of "empty" (`Frame::is_empty()`), used by both the TUI's placeholder and `dash9 test`'s `allow_empty` check, rather than each caller re-deriving it.

## Consequences
A future SQL-like or Loki adapter plugs in beside `dash9-prom` behind the same shape without the TUI or the test runner changing. The cost is that `Frame`'s `Table` kind is currently unused (only `dash9-prom`'s `vector`/`matrix`/`scalar` results exist today, all mapped to `Timeseries`/`InstantVector`) — it stays reserved rather than removed, since `SPEC.md`'s non-goals explicitly anticipate a future non-timeseries datasource needing it.
