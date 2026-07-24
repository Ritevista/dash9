# ADR 0003: One append-only command grammar for every surface

- Status: Accepted
- Date: 2026-07-18

## Context
TUI keybindings, an interactive command bar, dashboard TOML files, and a headless `dash9 test` runner all need to express the same underlying actions (add a datasource, run a query, set a panel's type or threshold). A bespoke API per surface would drift, and a future automated caller (see ADR 0008) would need to learn several APIs instead of one.

## Decision
One command grammar (`SPEC.md` Section B), parsed by a single `dash9-core::parse()`, drives every surface. The grammar is append-only starting at v0.1: a shipped verb's arity and semantics never change, and extending a capability means adding a new verb or subverb. Every parse or validation failure is a `CommandError` carrying a stable, append-only error code (`SPEC.md` Section B.5) — a code is never repurposed. `q`'s argument is a deliberate exception to the general tokenizer (raw-tail, no quote stripping) so PromQL's own quoting doesn't collide with the command grammar's.

## Consequences
Dashboard TOML files, saved keybindings, and anything a future automated caller has learned to emit remain valid forever. The cost is upfront rigor: a verb's arity can't be "fixed" later without shipping a new verb, and error codes accumulate rather than getting cleaned up — both are accepted trade-offs for long-term format stability.
