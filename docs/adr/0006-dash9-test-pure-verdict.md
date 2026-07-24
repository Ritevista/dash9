# ADR 0006: dash9 test — pure verdict logic separated from async I/O

- Status: Accepted
- Date: 2026-07-19

## Context
`SPEC.md` Section C.3's pass/fail rules (query executed, non-empty unless excused, within latency budget, with a specific tie-break when a panel is both empty and over budget) are exactly the kind of logic that's easy to get subtly wrong and hard to regression-test if it's inline in an async loop that also does real HTTP calls.

## Decision
The verdict itself is a pure, synchronous function in `dash9-core` — `check_panel(panel, query_result, elapsed_ms, dashboard_default_budget) -> PanelCheckResult` — that takes an already-obtained `Result<Frame, CommandError>` and an already-measured elapsed time, and performs no I/O. It reuses the command grammar's existing `E106` (`CommandError`) for a failed query rather than inventing a parallel error vocabulary. The `dash9` binary (`crates/dash9/src/test_cmd.rs`) owns everything with actual I/O: loading the dashboard, constructing one `PrometheusDatasource` per configured entry, calling the right `Datasource` method per panel type (`query_range` for `timeseries`, an instant `query` for `gauge`/`stat`/`table`), timing the call, and reporting `check_panel`'s verdict.

## Consequences
Every tie-break and edge case (empty-and-over-budget, a panel's own `latency_budget` overriding the dashboard default, exactly-at-budget not counting as exceeding) is unit-tested with zero network and zero async runtime involved. Regression coverage for the command as a whole still needs the async path exercised — done via black-box CLI tests (`crates/dash9/tests/cli.rs`) against a throwaway local TCP listener standing in for Prometheus, not a live network dependency. The cost is one more type (`PanelCheckResult`) and one more module boundary to keep in sync if `SPEC.md` C.3's rules ever change.
