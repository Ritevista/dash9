# dash9

dash9 is a terminal UI for observability dashboards: panels run queries against datasources and render as charts in the terminal. Dashboards are TOML files that are testable in CI, and every capability — from the interactive keybindings to the dashboard schema to the headless test runner — is driven by one shared command grammar.

> **Status:** early Phase 1 foundation. File formats and the command grammar may still change before the first stable release, though the grammar is designed to be append-only from v0.1 onward (`SPEC.md` Section B.1).

## Try it

Prerequisites: the latest stable Rust toolchain (currently 1.97.1) and Cargo. `rust-toolchain.toml` keeps contributors on the stable channel.

```console
cargo run -p dash9 -- demo
```

This runs a self-contained panel against synthetic data — a live braille line chart with axis labels, a legend, and threshold bands, falling back to a compact text view on narrow terminals. Press `q` or `Esc` to quit. No datasource or network access is involved.

Validate a real dashboard TOML file headlessly, the same way CI does:

```console
cargo run -p dash9 -- test path/to/dashboard.toml
```

`dash9 test` loads and validates the file, runs each panel's query against its configured Prometheus datasource, checks that the result is non-empty (unless the panel allows it) and within its latency budget, and exits `0`/`1`/`2` depending on whether every panel passed, a panel failed, or the file itself was invalid (`SPEC.md` Section C.3).

## Test against a live datasource

`docker-compose.yml` runs a real Prometheus scraping itself and a
`node_exporter`, so `dash9-prom` has live, changing metrics to query —
no mocks. Requires Docker.

```console
just up      # or: docker compose up -d
cargo run -p dash9 -- test examples/node-overview.toml
cargo run -p dash9 -- dash open examples/node-overview.toml
just down    # or: docker compose down
```

`node_exporter` reports on whatever the Docker daemon's kernel is —
the host's on Linux, the Docker Desktop VM's on macOS/Windows — either
way the metrics are real and change in real time, which is what
exercising the datasource port needs.

Prometheus is published on host port **9091**, not 9090 — some
systems (e.g. Cockpit) already bind 9090, and Docker publishes fail
silently rather than erroring in that case, so `examples/node-overview.toml` points at `9091`. If `9091` is also taken on your machine, change both the `ports:` mapping in `docker-compose.yml` and the datasource `url` in the example dashboard to a free port.

## Principles

- One `Frame` type is the boundary every datasource adapter produces and every renderer consumes — nothing downstream of a `Frame` knows which datasource produced it (`SPEC.md` Section A).
- One command grammar is shared by the TUI, the command bar, dashboard TOML files, and the headless test runner — never a bespoke API per surface (`SPEC.md` Section B).
- Presentation models know no widgets: chart/panel models store data, labels, thresholds, and semantic status, never terminal types or raw colors. Color is never load-bearing (`docs/architecture/rendering.md`).
- `dash9-core` has no UI, network, or async-runtime dependency. Concrete I/O and cross-crate wiring live only in the `dash9` binary.

Start with [`SPEC.md`](SPEC.md) for the Phase 1 data model, command grammar, and dashboard schema; [`docs/architecture/rendering.md`](docs/architecture/rendering.md) for the rendering pipeline and dependency boundaries; and [`docs/specs/`](docs/specs) for later-phase specifications. Contributors should read [CONTRIBUTING.md](CONTRIBUTING.md) and [AGENTS.md](AGENTS.md).

## Validation

```console
just check
just test
just ci
```

## License

Licensed under either [Apache License 2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT), at your option.
