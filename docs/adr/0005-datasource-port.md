# ADR 0005: Datasource port with Prometheus as the only v0.1 adapter

- Status: Accepted
- Date: 2026-07-19

## Context
`SPEC.md`'s non-goals are explicit that v0.1 supports no datasource beyond Prometheus, but the TUI, `dash9 test`, and any future automated caller should not be written against a Prometheus-specific API — a second backend (Loki, a SQL-like source) should be addable without touching them.

## Decision
`dash9-core` defines a `Datasource` trait (`query`, `query_range`) using native `async fn`-in-trait with an explicit `+ Send` bound on the returned futures and a `Self: Sync` supertrait bound, so the port stays runtime-agnostic — no `tokio` type appears in `dash9-core`'s signature, only `core::future::Future`. `dash9-prom::PrometheusDatasource` implements it, normalizing Prometheus's `vector`/`matrix`/`scalar` result shapes to `Frame` (ADR 0002) entirely inside its own crate. One `PrometheusDatasource` is constructed per named `[[datasources]]` entry; its `name` becomes every produced `Frame`'s `meta.datasource`.

## Consequences
A second datasource type implements the same trait and plugs in beside `dash9-prom` without `dash9-core` or `dash9-tui` changing. The cost of native async-fn-in-trait over a boxed-future or `async-trait`-macro approach is that the trait is not object-safe as written (no `dyn Datasource`) — acceptable today since v0.1 only ever constructs one concrete adapter type per named datasource and a `HashMap<String, PrometheusDatasource>` suffices; revisit if/when a second datasource *type* needs to coexist behind one dynamically-dispatched handle.
