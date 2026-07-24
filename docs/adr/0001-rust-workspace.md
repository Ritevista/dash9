# ADR 0001: Rust workspace and dependency baseline

- Status: Accepted
- Date: 2026-07-18

## Context
dash9 needs strong types shared across a data model, a terminal renderer, and one or more datasource adapters, plus a CLI, without letting any of those concerns leak into the others.

## Decision
Follow the latest stable Rust channel and record 1.97.1 as the initial MSRV. Use a workspace with `dash9-core` (Frame model, dashboard schema, command grammar), `dash9-prom` (Prometheus adapter), `dash9-tui` (Ratatui frontend), and `dash9` (binary/composition root) crates. `dash9-core` never depends on Ratatui, Crossterm, Tokio, reqwest, or any other UI/network/async-runtime crate; `dash9-tui` never depends on `dash9-prom`, reqwest, or Tokio. Both boundaries are enforced mechanically by `scripts/check-architecture.sh`, not by review alone. Use Serde/`toml` for the canonical dashboard schema, `thiserror` for library errors, Clap for the CLI, Anyhow only in the binary, Ratatui/Crossterm for the terminal, and Tokio (multi-threaded, feature-gated to what's used) for the async datasource adapters. Tests use `tempfile`, `assert_cmd`, `predicates`, and `proptest`.

## Consequences
Dependency direction is enforced by CI, not just convention, so a future contributor cannot accidentally give the domain crate a network or terminal dependency without the build failing. The cost is that any genuinely cross-cutting concern (e.g. a future shared time-formatting helper) must be placed carefully — usually in `dash9-core` if it's pure data/logic, never reached for from there if it needs I/O.
