# Repository instructions for coding agents

Read `SPEC.md`, the relevant files in `docs/specs/` and `docs/adr/`, and `docs/architecture/rendering.md` before editing. Behaviour changes require a specification update (`SPEC.md` for Phase 1, a `docs/specs/*.md` file for later phases); architectural changes require a new or superseding ADR. Keep changes narrow and reviewable.

## Non-negotiable boundaries

- `dash9-core` must not depend on Ratatui, Crossterm, Tokio, reqwest, or any other UI/network/async-runtime crate — enforced by `scripts/check-architecture.sh`.
- `dash9-tui` must not depend on `dash9-prom`, reqwest, or Tokio — same script. Terminal-specific projection stays in `dash9-tui`; concrete I/O and cross-crate conversions stay in the `dash9` binary (the composition root).
- Presentation models (`ChartModel`, and anything similar added later) store data, labels, thresholds, and semantic status only — never Ratatui types or raw colors. See `docs/architecture/rendering.md`.
- The command grammar (`SPEC.md` Section B) is append-only: a shipped verb's arity and semantics never change. Extending a capability means adding a new verb or subverb.
- Error codes (`SPEC.md` Section B.5) are stable for the lifetime of the grammar: a code is never repurposed, only appended to.
- If/when the `dash9-assist` crate lands (`docs/specs/assist.md`), it has exactly one effector — emitting command-grammar text — and no network access beyond its own configured LLM endpoint. It must not depend on `dash9-tui` or `dash9-prom`.
- Do not silently change the public CLI, the dashboard TOML schema, or any exported/persisted format.
- Production code must not use `unwrap`, `expect`, `panic!`, `todo!`, or `unimplemented!` for recoverable conditions. Unsafe Rust is forbidden (`unsafe_code = "forbid"` at the workspace level).
- Tests must be deterministic, order-independent, and offline. Do not add tests that depend on a live network endpoint or a real Prometheus instance — use fixtures, or a throwaway local TCP listener the test itself owns (see `crates/dash9-prom/src/lib.rs`'s `live_tests` module and `crates/dash9/tests/cli.rs` for the established pattern).

## Required workflow

Add unit or integration coverage for every behavioural change and a regression test for every defect fix. Before finishing, run:

```console
just ci
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
```

which covers `cargo fmt --all --check`, `cargo check --workspace --all-targets --all-features`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `./scripts/check-architecture.sh`, and `cargo test --workspace --all-targets --all-features`. Summarize decisions, changed files, exact commands and results, and remaining risks. Leave the working tree clean.
