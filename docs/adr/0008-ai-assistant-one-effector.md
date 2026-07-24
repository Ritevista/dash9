# ADR 0008: AI assistant — one effector, command-grammar text only

- Status: Proposed
- Date: 2026-07-19

## Context
An LLM-backed assistant is useful for turning a natural-language request into dashboard actions, but an assistant with broad access (files, TUI state, arbitrary network calls) would reopen every safety and auditability question the command grammar (ADR 0003) was built to close. Full design detail lives in `docs/specs/assist.md`; this ADR records only the durable architectural boundary, ahead of implementation.

## Decision
The assistant has exactly one effector: it emits dash9 command-grammar text, validated by the same `dash9_core::parse()` every other command source goes through, then executed via the same read-only/state-changing execution policy a human's input would follow. If a capability cannot be expressed as a command, the assistant does not have that capability — there is no secondary, richer API. It lives in a new, optional crate (`dash9-assist`), behind a Cargo feature on the `dash9` binary, depending only on `dash9-core` (never `dash9-tui` or `dash9-prom`) plus its own LLM HTTP client. `dash9-core` gains zero new crate dependencies from this work — only new pure-data types (a machine-readable verb reference, a workspace-relative path check, a session-log entry shape) that any command source can use, not assist-specific additions bolted on the side. The assistant's only network access, ever, is its own configured OpenAI-compatible LLM endpoint; fetching anything else (datasource metadata, dashboard file contents) is the composition root's job, handed to the assistant as already-fetched data.

## Consequences
Every assistant-originated action is auditable by construction — it went through the exact same parser and error codes a malformed human command would, and lands in the same session log, marked as assistant-originated. The cost is that any assistant capability not expressible in the existing grammar requires a new verb (reviewed under ADR 0003's append-only rule) rather than a bespoke assistant-only code path — a deliberate constraint, not an oversight.
